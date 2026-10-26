//! Changed files as list rows: a status letter, the file's name, its folder muted after it, and
//! the lines the change adds and removes at the right.

use bootty_git::{changes::LineCounts, diff::FileDiff};
use gpui_kit::component::{ActiveTheme as _, list::ListItem, tree::TreeItem};
use gpui_kit::{
    App, Div, ElementId, FontWeight, Hsla, IntoElement, ParentElement as _, SharedString,
    Styled as _, div, prelude::FluentBuilder as _,
};
use std::collections::BTreeMap;

#[derive(Default)]
struct Folder {
    directories: BTreeMap<String, Self>,
    files: BTreeMap<String, SharedString>,
}

impl Folder {
    fn items(
        self,
        prefix: &str,
        expansion: &BTreeMap<SharedString, bool>,
        expanded: bool,
    ) -> Vec<TreeItem> {
        let mut items = Vec::new();
        for (mut name, mut folder) in self.directories {
            let mut path = format!("{prefix}{name}/");
            // Empty directory chains share a row.
            while folder.files.is_empty() && folder.directories.len() == 1 {
                let Some((child, nested)) = folder.directories.pop_first() else {
                    break;
                };
                name.push('/');
                name.push_str(&child);
                path.push_str(&child);
                path.push('/');
                folder = nested;
            }
            let id: SharedString = path.clone().into();
            let open = expansion.get(&id).copied().unwrap_or(expanded);
            items.push(
                TreeItem::new(id, name)
                    .children(folder.items(&path, expansion, expanded))
                    .expanded(open),
            );
        }
        items.extend(
            self.files
                .into_iter()
                .map(|(name, path)| TreeItem::new(path, name)),
        );
        items
    }
}

/// Group changed paths into sorted folders, compacting directory chains into one row.
pub fn change_tree(
    paths: impl IntoIterator<Item = SharedString>,
    expansion: &BTreeMap<SharedString, bool>,
    expanded: bool,
) -> Vec<TreeItem> {
    let mut root = Folder::default();
    for path in paths {
        let normalized = path.replace('\\', "/");
        let mut parts = normalized
            .split('/')
            .filter(|part| !part.is_empty())
            .peekable();
        let mut parent = &mut root;
        while let Some(part) = parts.next() {
            if parts.peek().is_none() {
                parent.files.insert(part.to_owned(), path.clone());
            } else {
                parent = parent.directories.entry(part.to_owned()).or_default();
            }
        }
    }
    root.items("", expansion, expanded)
}

/// What a change did to its file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChangeKind {
    Added,
    Modified,
    Deleted,
    Renamed,
    Conflicted,
}

impl ChangeKind {
    /// The kind Git's status letter names; an untracked file (`?`) is new.
    #[must_use]
    pub const fn from_status(letter: char) -> Self {
        match letter {
            'A' | '?' => Self::Added,
            'D' => Self::Deleted,
            'R' | 'C' => Self::Renamed,
            'U' => Self::Conflicted,
            _ => Self::Modified,
        }
    }

    /// The kind a patch shows: a file with only added lines is new, one with only removed lines
    /// is gone.
    #[must_use]
    pub fn of_diff(file: &FileDiff) -> Self {
        let lines = || file.hunks.iter().flat_map(|hunk| &hunk.lines);
        if file.previous_path.is_some() {
            Self::Renamed
        } else if lines().next().is_none() {
            Self::Modified
        } else if lines().all(|line| line.old_line.is_none()) {
            Self::Added
        } else if lines().all(|line| line.new_line.is_none()) {
            Self::Deleted
        } else {
            Self::Modified
        }
    }

    const fn letter(self) -> &'static str {
        match self {
            Self::Added => "A",
            Self::Modified => "M",
            Self::Deleted => "D",
            Self::Renamed => "R",
            Self::Conflicted => "!",
        }
    }

    /// The kind in words, for screen readers.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Added => "Added",
            Self::Modified => "Modified",
            Self::Deleted => "Deleted",
            Self::Renamed => "Renamed",
            Self::Conflicted => "Conflicted",
        }
    }

    fn color(self, cx: &App) -> Hsla {
        let theme = cx.theme();
        match self {
            Self::Added => theme.success,
            Self::Modified => theme.warning,
            Self::Deleted | Self::Conflicted => theme.danger,
            Self::Renamed => theme.info,
        }
    }
}

/// One changed file's row, with `trailing`, such as its line counts and actions, at the right.
/// Callers add what clicking it does and whether it is selected.
pub fn change_row(
    id: impl Into<ElementId>,
    kind: ChangeKind,
    path: &str,
    trailing: impl IntoElement,
    cx: &App,
) -> ListItem {
    let (folder, name) = path.rsplit_once('/').unwrap_or(("", path));
    ListItem::new(id)
        .accessibility_label(format!("{}: {path}", kind.name()))
        .w_full()
        .h_7()
        .px_2()
        .py_0()
        .text_sm()
        .child(
            div()
                .w_full()
                .flex()
                .items_center()
                .gap_2()
                .min_w_0()
                .child(
                    div()
                        .w_3()
                        .flex_shrink_0()
                        .text_xs()
                        .font_weight(FontWeight::SEMIBOLD)
                        .font_family(cx.theme().mono_font_family.clone())
                        .text_color(kind.color(cx))
                        .child(kind.letter()),
                )
                // The name keeps its room; the folder takes what is left.
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .child(SharedString::from(name.to_owned())),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(SharedString::from(folder.to_owned())),
                )
                .child(div().flex_shrink_0().child(trailing)),
        )
}

/// The lines a change adds and removes, like "+12 −3"; a side without lines is left out.
pub fn line_counts(counts: LineCounts, cx: &App) -> Div {
    div()
        .flex()
        .flex_shrink_0()
        .gap_1()
        .text_xs()
        .font_family(cx.theme().mono_font_family.clone())
        .when(counts.added > 0, |row| {
            row.child(
                div()
                    .text_color(cx.theme().success)
                    .child(format!("+{}", counts.added)),
            )
        })
        .when(counts.removed > 0, |row| {
            row.child(
                div()
                    .text_color(cx.theme().danger)
                    .child(format!("−{}", counts.removed)),
            )
        })
}
