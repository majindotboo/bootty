//! Expandable host-scoped file tree, sharing Git decorations with the Changes panel.
use crate::gpui_git_panel::{GitChangesPanel, GitPanelContext};
use bootty_control::{
    BoundAppCommandSender, Caller, CommandCancellation, CommandInvocation, CommandOutcome,
};
use bootty_host::files::{DirectoryPage, FileResponse};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, IconName, Sizable as _,
    button::{Button, ButtonVariants as _},
    dock::{BasePanel, Panel, PanelEvent, PanelInfo, PanelState},
    input::{Input, InputEvent, InputState},
    list::ListItem,
    tree::{Tree, TreeEvent, TreeItem, TreeState},
};
use gpui_kit::{
    App, Context, Entity, EventEmitter, FocusHandle, Focusable, IntoElement, ParentElement, Render,
    SharedString, Styled, Subscription, Window, div, prelude::*,
};
use num_traits::ToPrimitive as _;
use std::{
    collections::{BTreeMap, BTreeSet},
    time::{Duration, Instant},
};

pub struct OpenDocument(pub String);
pub struct FilesPanel {
    context: GitPanelContext,
    root: String,
    generation: u64,
    sender: BoundAppCommandSender,
    git: Entity<GitChangesPanel>,
    pages: BTreeMap<String, DirectoryPage>,
    expanded: BTreeSet<String>,
    filter: Entity<InputState>,
    errors: BTreeMap<String, String>,
    pending: BTreeSet<String>,
    tree: Entity<TreeState>,
    more: BTreeMap<String, (String, usize)>,

    _subscriptions: Vec<Subscription>,
}
impl FilesPanel {
    pub(crate) fn new(
        context: GitPanelContext,
        sender: BoundAppCommandSender,
        git: Entity<GitChangesPanel>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let filter = cx
            .new(|cx| InputState::new(window, cx).placeholder(crate::i18n::t(cx, "files-filter")));
        let tree = cx.new(|cx| TreeState::new(cx));
        let subscriptions = vec![
            cx.subscribe_in(&filter, window, |this, _, event, window, cx| match event {
                InputEvent::Change => this.sync_tree(cx),
                InputEvent::PressEnter { .. } => {
                    this.tree.update(cx, |tree, cx| {
                        if tree.selected_index().is_none() && tree.entry(0).is_some() {
                            tree.set_selected_index(Some(0), cx);
                        }
                        tree.focus(window, cx);
                    });
                }
                InputEvent::Focus | InputEvent::Blur => {}
            }),
            cx.subscribe_in(&tree, window, |this, _, event, window, cx| match event {
                TreeEvent::Expanded(path) => {
                    this.expanded.insert(path.to_string());
                    this.load(path.to_string(), 0, window, cx);
                    cx.emit(PanelEvent::LayoutChanged);
                }
                TreeEvent::Collapsed(path) => {
                    this.expanded.remove(path.as_ref());
                    cx.emit(PanelEvent::LayoutChanged);
                }
            }),
            cx.observe(&git, |_, _, cx| cx.notify()),
        ];
        let root = context.directory.clone();
        Self {
            context,
            root,
            generation: 0,
            sender,
            git,
            pages: BTreeMap::new(),
            expanded: BTreeSet::new(),
            filter,
            errors: BTreeMap::new(),
            pending: BTreeSet::new(),
            tree,
            more: BTreeMap::new(),

            _subscriptions: subscriptions,
        }
    }
    pub(crate) fn browse(&mut self, root: String, window: &Window, cx: &mut Context<Self>) {
        if self.root != root {
            self.root = root;
            self.generation = self.generation.wrapping_add(1);
            self.pages.clear();
            self.expanded.clear();
            self.errors.clear();
            self.pending.clear();
            self.sync_tree(cx);
            cx.emit(PanelEvent::LayoutChanged);
        }
        self.refresh(window, cx);
    }

    pub(crate) fn refresh(&mut self, window: &Window, cx: &mut Context<Self>) {
        self.load(self.root.clone(), 0, window, cx);
        for path in self.expanded.clone() {
            self.load(path, 0, window, cx);
        }
    }
    fn load(&mut self, path: String, offset: usize, window: &Window, cx: &mut Context<Self>) {
        if self.pending.contains(&path) {
            return;
        }
        let mut invocation = CommandInvocation::from_action("files.list", Caller::Internal);
        invocation.target = Some(self.context.target.clone());
        invocation.arguments = vec![path.clone(), offset.to_string()];
        let receiver = match self.sender.submit(
            invocation,
            Instant::now()
                .checked_add(Duration::from_mins(2))
                .unwrap_or_else(Instant::now),
            CommandCancellation::new(),
        ) {
            Ok(receiver) => receiver,
            Err(error) => {
                self.errors
                    .insert(path, format!("File command unavailable: {error:?}"));
                self.sync_tree(cx);
                return;
            }
        };
        self.pending.insert(path.clone());
        let generation = self.generation;
        cx.spawn_in(window, async move |weak, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { receiver.recv() })
                .await;
            _ = weak.update_in(cx, |this, _, cx| {
                if this.generation != generation {
                    return;
                }
                this.pending.remove(&path);
                match result {
                    Ok(CommandOutcome::Success { value, .. }) => {
                        match serde_json::from_value::<FileResponse>(value) {
                            Ok(FileResponse::Directory(mut page)) => {
                                if offset > 0
                                    && let Some(old) = this.pages.get(&path)
                                {
                                    let mut entries = old.entries.clone();
                                    entries.append(&mut page.entries);
                                    page.entries = entries;
                                }
                                this.errors.remove(&path);
                                this.pages.insert(path.clone(), page);
                            }
                            Ok(_) => {
                                this.errors
                                    .insert(path.clone(), "Unexpected file response".to_owned());
                            }
                            Err(error) => {
                                this.errors.insert(path.clone(), error.to_string());
                            }
                        }
                    }
                    Ok(outcome) => {
                        this.errors.insert(
                            path.clone(),
                            crate::commands::command_outcome_message(&outcome)
                                .unwrap_or_else(|| "Directory listing failed".to_owned()),
                        );
                    }
                    Err(error) => {
                        this.errors.insert(path.clone(), error.to_string());
                    }
                }
                this.sync_tree(cx);
            });
        })
        .detach();
        self.sync_tree(cx);
    }
    fn sync_tree(&mut self, cx: &mut Context<Self>) {
        let query = self.filter.read(cx).value().to_lowercase();
        self.more.clear();
        let items = self.items(&self.root.clone(), 0, &query, cx);
        self.tree.update(cx, |tree, cx| {
            let selected = tree.selected_item().cloned();
            tree.set_items(items, cx);
            if let Some(selected) = selected {
                let ix = tree.index_of(&selected.id);
                tree.set_selected_index(ix, cx);
            }
        });
        cx.notify();
    }

    fn items(&mut self, path: &str, depth: usize, query: &str, cx: &App) -> Vec<TreeItem> {
        // Bound pathological host directory nesting; deeper browsing remains available by root.
        if depth > 64 {
            return vec![
                TreeItem::new(
                    format!("note:{path}"),
                    crate::i18n::t(cx, "files-depth-limit"),
                )
                .disabled(true),
            ];
        }
        let Some(page) = self.pages.get(path).cloned() else {
            return vec![
                TreeItem::new(
                    format!("note:{path}"),
                    crate::i18n::t(
                        cx,
                        if self.pending.contains(path) {
                            "files-loading"
                        } else if self.errors.contains_key(path) {
                            "files-load-failed"
                        } else {
                            "files-expand"
                        },
                    ),
                )
                .disabled(true),
            ];
        };
        let mut items = Vec::new();
        for entry in page.entries {
            if !query.is_empty()
                && !entry.is_directory
                && !entry.name.to_lowercase().contains(query)
            {
                continue;
            }
            let mut item = TreeItem::new(
                entry.path.clone(),
                if entry.is_symlink {
                    format!("{} ↗", entry.name)
                } else {
                    entry.name
                },
            );
            if entry.is_directory {
                let children = self.items(&entry.path, depth.saturating_add(1), query, cx);
                item = item
                    .children(children)
                    .expanded(self.expanded.contains(&entry.path));
            }
            items.push(item);
        }
        if let Some(offset) = page.next_offset {
            let id = format!("more:{path}");
            self.more.insert(id.clone(), (path.to_owned(), offset));
            items.push(TreeItem::new(id, crate::i18n::t(cx, "files-more")));
        }
        if items.is_empty() {
            items.push(
                TreeItem::new(
                    format!("note:{path}"),
                    crate::i18n::t(
                        cx,
                        if query.is_empty() {
                            "files-empty"
                        } else {
                            "files-no-matches"
                        },
                    ),
                )
                .disabled(true),
            );
        }
        items
    }

    fn open_item(&mut self, id: &str, window: &Window, cx: &mut Context<Self>) {
        if let Some((path, offset)) = self.more.get(id).cloned() {
            self.load(path, offset, window, cx);
        } else if !id.starts_with("note:") {
            cx.emit(OpenDocument(id.to_owned()));
        }
    }
}
impl EventEmitter<PanelEvent> for FilesPanel {}
impl EventEmitter<OpenDocument> for FilesPanel {}
impl Focusable for FilesPanel {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.filter.focus_handle(cx)
    }
}
impl BasePanel for FilesPanel {
    fn panel_name(&self) -> &'static str {
        "bootty.files"
    }
    fn dump(&self, _: &App) -> PanelState {
        PanelState {
            panel_name: self.panel_name().to_owned(),
            children: Vec::new(),
            info: PanelInfo::panel(
                serde_json::json!({"host":self.context.host_identity, "root":self.root, "expanded":self.expanded}),
            ),
        }
    }
    fn set_active(&mut self, active: bool, window: &mut Window, cx: &mut Context<Self>) {
        if active {
            self.refresh(window, cx);
        }
    }
}
impl Panel for FilesPanel {
    fn inner_padding(&self, _: &App) -> bool {
        false
    }
    fn tab_name(&self, cx: &App) -> Option<SharedString> {
        Some(crate::i18n::t(cx, "panel-files").into())
    }
    fn title(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        crate::i18n::t(cx, "panel-files")
    }
}
impl Render for FilesPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let owner = cx.weak_entity();
        let git = self.git.clone();
        let mut body = self.render_header(cx);
        if let Some(error) = self.errors.get(&self.root) {
            body = body.child(gpui_kit::component::alert::Alert::error(
                "files-error",
                error.clone(),
            ));
        }
        let tree = Tree::new(&self.tree, move |_, entry, selected, _, cx| {
            let item = entry.item();
            let directory = entry.is_folder();
            let icon = if directory {
                if entry.is_expanded() {
                    IconName::FolderOpen
                } else {
                    IconName::Folder
                }
            } else {
                IconName::File
            };
            let decoration = git.read(cx).decoration(&item.id);
            let id = item.id.clone();
            let owner = owner.clone();
            ListItem::new(item.id.clone())
                .role(gpui_kit::Role::TreeItem)
                .aria_label(item.label.clone())
                .aria_selected(selected)
                .w_full()
                .h_6()
                .py_0()
                .text_sm()
                .px_2()
                .pl(gpui_kit::rems(
                    entry
                        .depth()
                        .to_f32()
                        .unwrap_or(f32::MAX)
                        .mul_add(0.75, 0.5),
                ))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_1()
                        .min_w_0()
                        .child(div().w_3().flex_shrink_0().when(directory, |slot| {
                            slot.child(
                                Icon::new(if entry.is_expanded() {
                                    IconName::ChevronDown
                                } else {
                                    IconName::ChevronRight
                                })
                                .xsmall(),
                            )
                        }))
                        .child(
                            Icon::new(icon)
                                .small()
                                .text_color(cx.theme().muted_foreground),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .child(item.label.clone()),
                        )
                        .child(
                            div()
                                .text_color(cx.theme().muted_foreground)
                                .child(decoration),
                        ),
                )
                .on_click(move |_, window, cx| {
                    if !directory {
                        _ = owner.update(cx, |this, cx| this.open_item(&id, window, cx));
                    }
                })
        });
        let selected_tree = self.tree.clone();
        let focus_tree = self.tree.clone();
        body.child(
            div()
                .id("file-tree")
                .flex_1()
                .min_h_0()
                .min_w_0()
                .role(gpui_kit::Role::Tree)
                .aria_label(crate::i18n::t(cx, "panel-files"))
                .on_mouse_down(gpui_kit::MouseButton::Left, move |_, window, cx| {
                    focus_tree.update(cx, |tree, cx| tree.focus(window, cx));
                })
                .on_key_down(cx.listener(
                    move |this, event: &gpui_kit::KeyDownEvent, window, cx| {
                        this.handle_tree_key(event, &selected_tree, window, cx);
                    },
                ))
                .child(tree),
        )
    }
}

impl FilesPanel {
    fn render_header(&self, cx: &Context<Self>) -> gpui_kit::Div {
        let root_name = self
            .root
            .trim_end_matches(['/', '\\'])
            .rsplit(['/', '\\'])
            .next()
            .filter(|name| !name.is_empty())
            .unwrap_or(&self.root)
            .to_owned();
        let parent = self
            .pages
            .get(&self.root)
            .and_then(|page| page.parent.clone());
        let pending = self.pending.contains(&self.root);
        div()
            .size_full()
            .flex()
            .flex_col()
            .min_w_0()
            .min_h_0()
            .child(
                div().px_2().py_1().child(crate::gpui::focus_input(
                    &self.filter,
                    Input::new(&self.filter)
                        .small()
                        .appearance(false)
                        .prefix(Icon::new(IconName::Search).small()),
                )),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .px_2()
                    .pb_1()
                    .child(Icon::new(IconName::FolderOpen).small())
                    .child(
                        div()
                            .id("files-root")
                            .flex_1()
                            .min_w_0()
                            .text_sm()
                            .truncate()
                            .tooltip({
                                let path = format!("{} · {}", self.context.host, self.root);
                                move |window, cx| {
                                    gpui_kit::component::tooltip::Tooltip::new(path.clone())
                                        .build(window, cx)
                                }
                            })
                            .child(root_name),
                    )
                    .when_some(parent, |row, parent| {
                        row.child(
                            Button::new("parent-directory")
                                .icon(IconName::ArrowUp)
                                .ghost()
                                .small()
                                .accessibility_label(crate::i18n::t(cx, "files-parent"))
                                .tooltip(crate::i18n::t(cx, "files-parent"))
                                .disabled(pending)
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.browse(parent.clone(), window, cx);
                                })),
                        )
                    })
                    .child(
                        Button::new("refresh-files")
                            .icon(IconName::RotateCw)
                            .ghost()
                            .small()
                            .accessibility_label(crate::i18n::t(cx, "common-refresh"))
                            .tooltip(crate::i18n::t(cx, "common-refresh"))
                            .loading(pending)
                            .disabled(pending)
                            .on_click(cx.listener(|this, _, window, cx| this.refresh(window, cx))),
                    ),
            )
    }
    fn handle_tree_key(
        &mut self,
        event: &gpui_kit::KeyDownEvent,
        selected_tree: &Entity<TreeState>,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        if event.keystroke.key == "enter" {
            let selected = selected_tree.read(cx).selected_entry().cloned();
            if let Some(entry) = selected.filter(|entry| !entry.is_disabled()) {
                let id = entry.item().id.to_string();
                if entry.is_folder() {
                    if self.expanded.remove(&id) {
                        self.sync_tree(cx);
                    } else {
                        self.expanded.insert(id.clone());
                        self.load(id, 0, window, cx);
                    }
                    cx.emit(PanelEvent::LayoutChanged);
                } else {
                    self.open_item(&id, window, cx);
                }
                cx.stop_propagation();
            }
        }
    }
}
