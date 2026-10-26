//! Changed-file navigation uses Kit's keyboard-accessible tree, preserving expansion by path.
use gpui_kit::component::{
    ActiveTheme as _, Icon, IconName, Sizable as _, StyledExt as _,
    button::{Button, ButtonVariants as _},
    diff::{DiffFile, DiffFileStatus},
    list::ListItem,
    tree::{Tree, TreeEntry, TreeItem, TreeState},
};
use gpui_kit::{
    App, AppContext as _, Context, Entity, EventEmitter, IntoElement, ParentElement as _, Render,
    SharedString, Styled as _, Subscription, WeakEntity, div, prelude::*,
};
use num_traits::ToPrimitive as _;
use std::collections::BTreeMap;

pub(super) struct SelectFile(pub SharedString);

pub(super) struct DiffFiles {
    tree: Entity<TreeState>,
    roots: Vec<TreeItem>,
    files: BTreeMap<SharedString, DiffFileStatus>,
    selected: Option<SharedString>,
    _selection: Subscription,
}

impl EventEmitter<SelectFile> for DiffFiles {}

fn expansions(items: &[TreeItem], result: &mut BTreeMap<SharedString, bool>) {
    for item in items {
        if item.is_folder() {
            result.insert(item.id.clone(), item.is_expanded());
            expansions(&item.children, result);
        }
    }
}

impl DiffFiles {
    pub(super) fn new(cx: &mut Context<Self>) -> Self {
        let tree = cx.new(|cx| TreeState::new(cx));
        let selection = cx.observe(&tree, |this, tree, cx| {
            let selected = tree
                .read(cx)
                .selected_item()
                .filter(|item| !item.is_folder())
                .map(|item| item.id.clone());
            if this.selected != selected {
                this.selected.clone_from(&selected);
                if let Some(path) = selected {
                    cx.emit(SelectFile(path));
                }
            }
            cx.notify();
        });
        Self {
            tree,
            roots: Vec::new(),
            files: BTreeMap::new(),
            selected: None,
            _selection: selection,
        }
    }

    pub(super) fn set_files(&mut self, files: &[DiffFile], cx: &mut Context<Self>) {
        let mut expansion = BTreeMap::new();
        expansions(&self.roots, &mut expansion);
        self.files.clear();
        for file in files {
            self.files.insert(file.path().clone(), file.status());
        }
        self.roots = crate::gpui::change_tree(self.files.keys().cloned(), &expansion, true);
        self.tree.update(cx, |tree, cx| {
            let selected = tree.selected_item().cloned();
            tree.set_items(self.roots.clone(), cx);
            if let Some(item) = selected {
                tree.set_selected_item(Some(&item), cx);
            }
        });
        cx.notify();
    }

    fn toggle_folders(&self, expand: bool, cx: &mut Context<Self>) {
        fn set(items: &[TreeItem], expand: bool) {
            for item in items {
                _ = item.clone().expanded(expand);
                set(&item.children, expand);
            }
        }
        set(&self.roots, expand);
        self.tree.update(cx, |tree, cx| {
            tree.set_items(self.roots.clone(), cx);
        });
        cx.notify();
    }
}

impl DiffFiles {
    fn row(
        entry: &TreeEntry,
        selected: bool,
        status: Option<DiffFileStatus>,
        owner: WeakEntity<Self>,
        cx: &App,
    ) -> ListItem {
        let item = entry.item();
        let directory = entry.is_folder();
        let path = item.id.clone();
        let (mark, color) = match status {
            Some(DiffFileStatus::Added) => ("A", cx.theme().success),
            Some(DiffFileStatus::Deleted) => ("D", cx.theme().danger),
            Some(DiffFileStatus::Renamed) => ("R", cx.theme().warning),
            Some(DiffFileStatus::Copied) => ("C", cx.theme().success),
            Some(DiffFileStatus::Modified | DiffFileStatus::Conflicted) => {
                ("M", cx.theme().warning)
            }
            _ => ("", cx.theme().muted_foreground),
        };
        let icon = if directory {
            Icon::new(if entry.is_expanded() {
                IconName::FolderOpen
            } else {
                IconName::Folder
            })
        } else {
            Icon::default().path(format!(
                "icons/{}.svg",
                crate::gpui_prompt_attachments::icon(&item.label)
            ))
        };
        ListItem::new(item.id.clone())
            .role(gpui_kit::Role::TreeItem)
            .aria_label(item.id.clone())
            .aria_selected(selected)
            .debug_selector({
                let path = path.clone();
                move || format!("git-diff-file:{path}")
            })
            .w_full()
            .h_6()
            .py_0()
            .px_2()
            .text_xs()
            .pl(gpui_kit::rems(
                entry.depth().to_f32().unwrap_or(0.).mul_add(0.75, 0.5),
            ))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .min_w_0()
                    .child(div().w_3().flex_none().when(directory, |slot| {
                        slot.child(
                            Icon::new(if entry.is_expanded() {
                                IconName::ChevronDown
                            } else {
                                IconName::ChevronRight
                            })
                            .xsmall(),
                        )
                    }))
                    .child(icon.small().text_color(cx.theme().muted_foreground))
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .truncate()
                            .child(item.label.clone()),
                    )
                    .child(div().flex_none().text_xs().text_color(color).child(mark)),
            )
            .tooltip({
                let path = path.to_string();
                move |window, cx| {
                    gpui_kit::component::tooltip::Tooltip::new(path.clone()).build(window, cx)
                }
            })
            .on_click(move |_, _, cx| {
                if !directory {
                    _ = owner.update(cx, |_, cx| cx.emit(SelectFile(path.clone())));
                }
            })
    }
}

impl Render for DiffFiles {
    fn render(&mut self, _: &mut gpui_kit::Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut expansion = BTreeMap::new();
        expansions(&self.roots, &mut expansion);
        let expanded = expansion.values().all(|open| *open);
        let owner = cx.weak_entity();
        let files = self.files.clone();
        let tree = Tree::new(&self.tree, move |_, entry, selected, _, cx| {
            Self::row(
                entry,
                selected,
                files.get(&entry.item().id).copied(),
                owner.clone(),
                cx,
            )
        });
        let focus = self.tree.clone();
        div()
            .id("git-diff-files")
            .debug_selector(|| "git-diff-files".into())
            .w(gpui_kit::relative(0.4))
            .min_w_40()
            .max_w_64()
            .h_full()
            .flex_none()
            .min_h_0()
            .flex()
            .flex_col()
            .border_l_1()
            .border_color(cx.theme().border)
            .child(
                div()
                    .h_9()
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap_1()
                    .px_2()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(div().text_xs().font_medium().child("Files"))
                    .child(div().flex_1())
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(self.files.len().to_string()),
                    )
                    .when(!expansion.is_empty(), |header| {
                        header.child(
                            Button::new("diff-folders")
                                .ghost()
                                .xsmall()
                                .icon(Icon::default().path(if expanded {
                                    "icons/chevrons-down-up.svg"
                                } else {
                                    "icons/chevrons-up-down.svg"
                                }))
                                .accessibility_label(if expanded {
                                    "Collapse all folders"
                                } else {
                                    "Expand all folders"
                                })
                                .tooltip(if expanded {
                                    "Collapse all folders"
                                } else {
                                    "Expand all folders"
                                })
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.toggle_folders(!expanded, cx);
                                })),
                        )
                    }),
            )
            .child(
                div()
                    .id("diff-files-tree")
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .py_1()
                    .role(gpui_kit::Role::Tree)
                    .aria_label("Changed files")
                    .on_mouse_down(gpui_kit::MouseButton::Left, move |_, window, cx| {
                        focus.update(cx, |tree, cx| tree.focus(window, cx));
                    })
                    .child(tree),
            )
    }
}
