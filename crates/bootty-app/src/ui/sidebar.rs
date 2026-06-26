use std::collections::HashMap;

use eframe::egui::Color32;

use crate::{
    extensions::{ModuleItem, ModulePrimitive},
    mux::snapshot::MuxSession,
};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SidebarState {
    pub focused: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum SidebarItemKind {
    Group,
    Session { active: bool },
    Row,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SidebarItemId<'a> {
    Group(&'a str),
    Session(&'a str),
    Row(&'a str),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SidebarDisplay<'a> {
    Text(&'a str),
    Numbered { number: usize, label: &'a str },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SidebarTree {
    None,
    Middle,
    Last,
    Pipe,
    Blank,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SidebarItem<'a> {
    pub id: SidebarItemId<'a>,
    pub display: SidebarDisplay<'a>,
    pub indent: u16,
    pub tree: SidebarTree,
    pub selectable: bool,
    pub session_id: Option<&'a str>,
    pub reorder_anchor: Option<&'a str>,
    pub color: Color32,
    pub dim_color: Color32,
    pub kind: SidebarItemKind,
    pub current: bool,
    pub icon: Option<&'a str>,
    pub primitives: &'a [ModulePrimitive],
}

pub fn build_sidebar_items<'a>(
    sessions: &'a [MuxSession],
    selected_session: Option<&str>,
) -> Vec<SidebarItem<'a>> {
    build_sidebar_items_inner(sessions, selected_session, None)
}

pub fn build_visible_sidebar_items<'a>(
    sessions: &'a [MuxSession],
    selected_session: Option<&str>,
    max_rows: usize,
) -> Vec<SidebarItem<'a>> {
    build_sidebar_items_inner(sessions, selected_session, Some(max_rows))
}

pub fn build_sidebar_items_from_module_items<'a>(
    items: &'a [ModuleItem],
    selected_session: Option<&str>,
) -> Vec<SidebarItem<'a>> {
    items
        .iter()
        .filter_map(|item| sidebar_item_from_module_item(item, selected_session))
        .collect()
}

fn sidebar_item_from_module_item<'a>(
    item: &'a ModuleItem,
    selected_session: Option<&str>,
) -> Option<SidebarItem<'a>> {
    let kind = item.kind.as_deref().unwrap_or("row");
    if kind == "footer" {
        return None;
    }
    let row_key = item.key.as_deref().unwrap_or_else(|| {
        if kind == "session" {
            item.session_id.as_deref().unwrap_or(item.text.as_str())
        } else {
            item.text.as_str()
        }
    });
    let display = if let Some(number) = item.number {
        SidebarDisplay::Numbered {
            number,
            label: item.text.as_str(),
        }
    } else {
        SidebarDisplay::Text(item.text.as_str())
    };
    let selected = selected_session.is_some_and(|selected| {
        item.session_id.as_deref() == Some(selected) || item.text == selected
    });
    let selectable = item.selectable.unwrap_or(kind == "session");
    let current = if selectable && selected_session.is_some() {
        selected
    } else {
        item.current.unwrap_or(false)
    };
    let sidebar_kind = match kind {
        "group" => SidebarItemKind::Group,
        "session" => SidebarItemKind::Session {
            active: selected_session.map_or(item.active.unwrap_or(current), |_| current),
        },
        _ => SidebarItemKind::Row,
    };
    let color = item.fg.unwrap_or(Color32::WHITE);
    Some(SidebarItem {
        id: sidebar_item_id(kind, row_key, item.text.as_str()),
        display,
        indent: item.indent.unwrap_or(0),
        tree: sidebar_tree(item.tree.as_deref()),
        selectable,
        session_id: item.session_id.as_deref(),
        reorder_anchor: item.reorder_anchor.as_deref(),
        color,
        dim_color: item.dim_fg.unwrap_or(color),
        kind: sidebar_kind,
        current,
        icon: item.icon.as_deref(),
        primitives: &item.primitives,
    })
}

fn sidebar_item_id<'a>(kind: &str, row_key: &'a str, text: &'a str) -> SidebarItemId<'a> {
    match kind {
        "group" => SidebarItemId::Group(text),
        "session" => SidebarItemId::Session(row_key),
        _ => SidebarItemId::Row(row_key),
    }
}

fn sidebar_tree(value: Option<&str>) -> SidebarTree {
    match value {
        Some("middle") => SidebarTree::Middle,
        Some("last") => SidebarTree::Last,
        Some("pipe") => SidebarTree::Pipe,
        Some("blank") => SidebarTree::Blank,
        _ => SidebarTree::None,
    }
}

fn build_sidebar_items_inner<'a>(
    sessions: &'a [MuxSession],
    selected_session: Option<&str>,
    max_rows: Option<usize>,
) -> Vec<SidebarItem<'a>> {
    if max_rows == Some(0) {
        return Vec::new();
    }
    let mut group_meta = GroupMeta::new(sessions);
    let full_capacity = sessions.len().saturating_mul(6);
    let mut items = Vec::with_capacity(max_rows.unwrap_or(full_capacity).min(full_capacity));
    let mut ordinal = 0usize;
    let mut last_group = "";

    for (index, session) in sessions.iter().enumerate() {
        let Some(group_info) = group_meta.session(index) else {
            continue;
        };
        let group = group_info.name;
        let group_index = group_info.index;
        let group_count = group_info.count;
        let group_total = if group.is_empty() { 0 } else { group_count };
        let is_grouped = !group.is_empty() && group_total > 1;
        let is_last_in_group =
            is_grouped && group_meta.session_group_index(index + 1) != Some(group_index);
        let session_tree = if !is_grouped {
            SidebarTree::None
        } else if is_last_in_group {
            SidebarTree::Last
        } else {
            SidebarTree::Middle
        };

        let (color, dim_color) = computed_color(
            group_index,
            group_meta.dynamic_total,
            group_info.position,
            group_total,
        );
        let selected = if selected_session.is_some() {
            selected_session == Some(session.id.as_str())
                || selected_session == Some(session.name.as_str())
        } else {
            session.active
        };
        let reorder_anchor = if is_grouped {
            group_info.leader_session
        } else {
            session.name.as_str()
        };
        let (display, session_indent) = if is_grouped {
            if group != last_group {
                items.push(SidebarItem {
                    id: SidebarItemId::Group(group),
                    display: SidebarDisplay::Text(group),
                    indent: 0,
                    tree: SidebarTree::None,
                    selectable: false,
                    session_id: None,
                    reorder_anchor: Some(reorder_anchor),
                    color,
                    dim_color,
                    kind: SidebarItemKind::Group,
                    current: false,
                    icon: None,
                    primitives: &[],
                });
                if max_rows.is_some_and(|limit| items.len() >= limit) {
                    break;
                }
            }
            let suffix = session_suffix(&session.name);
            let label = if suffix.is_empty() { group } else { suffix };
            let display = SidebarDisplay::Numbered {
                number: ordinal + 1,
                label,
            };
            ordinal += 1;
            (display, 2)
        } else {
            let label = if group.is_empty() {
                session.name.as_str()
            } else {
                group
            };
            let display = SidebarDisplay::Numbered {
                number: ordinal + 1,
                label,
            };
            ordinal += 1;
            (display, 0)
        };

        items.push(SidebarItem {
            id: SidebarItemId::Session(session.id.as_str()),
            display,
            indent: session_indent,
            tree: session_tree,
            selectable: true,
            session_id: Some(session.id.as_str()),
            reorder_anchor: Some(reorder_anchor),
            color,
            dim_color,
            kind: SidebarItemKind::Session { active: selected },
            current: selected,
            icon: None,
            primitives: &[],
        });
        if max_rows.is_some_and(|limit| items.len() >= limit) {
            break;
        }

        last_group = group;
    }

    items
}

pub fn session_group(name: &str) -> &str {
    name.split_once('/').map_or(name, |(group, _)| group)
}

pub fn session_suffix(name: &str) -> &str {
    name.split_once('/').map_or("", |(_, suffix)| suffix)
}

#[derive(Debug)]
struct GroupSummary<'a> {
    name: &'a str,
    leader_session: &'a str,
    count: usize,
    position: usize,
}

#[derive(Debug)]
struct GroupSession<'a> {
    name: &'a str,
    leader_session: &'a str,
    index: usize,
    count: usize,
    position: usize,
}

struct GroupMeta<'a> {
    groups: Vec<GroupSummary<'a>>,
    session_groups: Vec<usize>,
    dynamic_total: usize,
}

const HASHED_GROUP_LOOKUP_THRESHOLD: usize = 32;

impl<'a> GroupMeta<'a> {
    fn new(sessions: &'a [MuxSession]) -> Self {
        let mut groups = Vec::<GroupSummary<'a>>::new();
        let mut session_groups = Vec::with_capacity(sessions.len());
        let mut lookup = None::<HashMap<&'a str, usize>>;
        for session in sessions {
            let group = session_group(&session.name);
            if let Some(index) = lookup
                .as_ref()
                .and_then(|lookup| lookup.get(group).copied())
            {
                groups[index].count += 1;
                session_groups.push(index);
                continue;
            }
            if lookup.is_none()
                && let Some((index, existing)) = groups
                    .iter_mut()
                    .enumerate()
                    .find(|(_, summary)| summary.name == group)
            {
                existing.count += 1;
                session_groups.push(index);
                continue;
            }

            let index = groups.len();
            groups.push(GroupSummary {
                name: group,
                leader_session: session.name.as_str(),
                count: 1,
                position: 0,
            });
            session_groups.push(index);
            if let Some(lookup) = &mut lookup {
                lookup.insert(group, index);
            } else if groups.len() > HASHED_GROUP_LOOKUP_THRESHOLD {
                lookup = Some(
                    groups
                        .iter()
                        .enumerate()
                        .map(|(index, summary)| (summary.name, index))
                        .collect(),
                );
            }
        }
        let dynamic_total = groups.len();
        Self {
            groups,
            session_groups,
            dynamic_total,
        }
    }

    fn session_group_index(&self, index: usize) -> Option<usize> {
        self.session_groups.get(index).copied()
    }

    fn session(&mut self, index: usize) -> Option<GroupSession<'a>> {
        let group_index = self.session_group_index(index)?;
        let summary = self.groups.get_mut(group_index)?;
        let position = summary.position;
        if !summary.name.is_empty() {
            summary.position += 1;
        }
        Some(GroupSession {
            name: summary.name,
            leader_session: summary.leader_session,
            index: group_index,
            count: summary.count,
            position,
        })
    }
}

fn computed_color(
    pos: usize,
    total: usize,
    group_pos: usize,
    group_total: usize,
) -> (Color32, Color32) {
    let base = if total > 0 {
        60.0 + (pos as f64 * 300.0) / total as f64
    } else {
        210.0
    };
    let (hue, lightness) = if group_total > 1 {
        let t = group_pos as f64 / (group_total - 1) as f64;
        (
            (base + (t * 60.0 - 30.0) + 360.0) % 360.0,
            0.55 + (t - 0.5) * 0.15,
        )
    } else {
        (base, 0.6)
    };
    (
        hsl_to_color(hue, 0.55, lightness),
        hsl_to_color(hue, 0.2, 0.45),
    )
}

fn hsl_to_color(hue: f64, saturation: f64, lightness: f64) -> Color32 {
    let c = (1.0 - (2.0 * lightness - 1.0).abs()) * saturation;
    let hp = hue / 60.0;
    let x = c * (1.0 - (hp % 2.0 - 1.0).abs());
    let m = lightness - c / 2.0;
    let (r, g, b) = match hp as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    Color32::from_rgb(
        ((r + m) * 255.0) as u8,
        ((g + m) * 255.0) as u8,
        ((b + m) * 255.0) as u8,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mux::snapshot::MuxPaneAnchor;

    #[test]
    fn extension_items_build_sidebar_rows_and_footer_items_stay_generic() {
        let primitive = ModulePrimitive::Text {
            text: "right".to_owned(),
            color: Some(Color32::from_rgb(0xa6, 0xe3, 0xa1)),
            x: crate::extensions::ModuleCoord {
                frac: 1.0,
                px: -8.0,
            },
            y: crate::extensions::ModuleCoord { frac: 0.5, px: 0.0 },
            size: 11.0,
            align: "right_center".to_owned(),
            min_width: None,
        };
        let items = vec![
            ModuleItem {
                kind: Some("session".to_owned()),
                text: "api".to_owned(),
                number: Some(1),
                session_id: Some("$1".to_owned()),
                reorder_anchor: Some("work/api".to_owned()),
                fg: Some(Color32::from_rgb(0x89, 0xb4, 0xfa)),
                dim_fg: Some(Color32::from_rgb(0x45, 0x5a, 0x7d)),
                current: Some(true),
                active: Some(true),
                primitives: vec![primitive.clone()],
                ..ModuleItem::default()
            },
            ModuleItem {
                kind: Some("footer".to_owned()),
                text: "codex".to_owned(),
                primitives: vec![primitive],
                ..ModuleItem::default()
            },
        ];

        let rows = build_sidebar_items_from_module_items(&items, None);

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].session_id, Some("$1"));
        assert_eq!(
            rows[0].display,
            SidebarDisplay::Numbered {
                number: 1,
                label: "api"
            }
        );
        assert!(rows[0].current);
        assert!(matches!(
            rows[0].kind,
            SidebarItemKind::Session { active: true }
        ));
        assert_eq!(rows[0].primitives.len(), 1);
    }

    #[test]
    fn extension_session_selection_uses_bootty_selected_session_without_waiting_for_luau() {
        let items = vec![
            ModuleItem {
                kind: Some("session".to_owned()),
                text: "one".to_owned(),
                session_id: Some("$1".to_owned()),
                current: Some(true),
                active: Some(true),
                selectable: Some(true),
                ..ModuleItem::default()
            },
            ModuleItem {
                kind: Some("session".to_owned()),
                text: "two".to_owned(),
                session_id: Some("$2".to_owned()),
                current: Some(false),
                active: Some(false),
                selectable: Some(true),
                ..ModuleItem::default()
            },
        ];

        let rows = build_sidebar_items_from_module_items(&items, Some("$2"));

        assert!(!rows[0].current);
        assert!(rows[1].current);
        assert!(matches!(
            rows[1].kind,
            SidebarItemKind::Session { active: true, .. }
        ));
    }

    #[test]
    fn groups_sessions_without_luau_enrichment_rows() {
        let sessions = vec![
            session("$1", "work/api", "zsh"),
            session("$2", "work/ui", "nvim"),
        ];

        let items = build_sidebar_items(&sessions, Some("$1"));
        assert_eq!(
            items[1].display,
            SidebarDisplay::Numbered {
                number: 1,
                label: "api"
            }
        );
        assert!(matches!(items[1].kind, SidebarItemKind::Session { .. }));
        assert_eq!(items.len(), 3);
        assert_eq!(
            items[2].display,
            SidebarDisplay::Numbered {
                number: 2,
                label: "ui"
            }
        );
        assert_eq!(items[1].tree, SidebarTree::Middle);
        assert_eq!(items[2].tree, SidebarTree::Last);
    }

    #[test]
    fn selected_session_does_not_also_mark_attached_session_current() {
        let mut sessions = vec![session("$1", "one", "zsh"), session("$2", "two", "fish")];
        sessions[0].active = true;

        let items = build_sidebar_items(&sessions, Some("$2"));

        let current = items
            .iter()
            .filter(|item| matches!(item.kind, SidebarItemKind::Session { .. }) && item.current)
            .map(|item| item.session_id)
            .collect::<Vec<_>>();
        assert_eq!(current, vec![Some("$2")]);
    }

    #[test]
    fn visible_sidebar_items_match_full_prefix() {
        let sessions = vec![
            session("$1", "work/api", "zsh"),
            session("$2", "work/ui", "nvim"),
            session("$3", "work/bench", "cargo"),
            session("$4", "ops/logs", "tail"),
        ];
        let full = build_sidebar_items(&sessions, Some("$2"));
        let visible = build_visible_sidebar_items(&sessions, Some("$2"), 5);

        assert_eq!(visible.as_slice(), &full[..5]);
    }

    #[test]
    fn visible_sidebar_items_match_full_prefix_with_many_groups() {
        let sessions = (0..40)
            .map(|index| {
                session(
                    &format!("${index}"),
                    &format!("group-{index}/session"),
                    "zsh",
                )
            })
            .collect::<Vec<_>>();
        let full = build_sidebar_items(&sessions, Some("$2"));
        let visible = build_visible_sidebar_items(&sessions, Some("$2"), 17);

        assert_eq!(visible.as_slice(), &full[..17]);
    }

    #[test]
    fn session_group_uses_first_slash() {
        assert_eq!(session_group("a/b/c"), "a");
        assert_eq!(session_suffix("a/b/c"), "b/c");
    }

    fn session(id: &str, name: &str, process: &str) -> MuxSession {
        MuxSession {
            id: id.to_owned(),
            name: name.to_owned(),
            active: false,
            anchor: MuxPaneAnchor {
                session_id: id.to_owned(),
                pane_id: None,
                cwd: None,
                process: Some(process.to_owned()),
            },
            active_window_id: None,
            windows: Vec::new(),
        }
    }
}
