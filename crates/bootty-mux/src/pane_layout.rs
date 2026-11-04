//! Canonical server-owned pane layout and geometry for directional navigation.

use crate::snapshot::{MuxPaneLayout, MuxPaneSplitDirection};
use bootty_terminal::geometry::SurfaceRect;
use serde::{Deserialize, Deserializer};

pub type PaneId = String;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SplitDirection {
    Right,
    Down,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Left,
    Right,
    Up,
    Down,
}

// Only the read boundary knows the retired binary representation.
#[derive(Deserialize)]
enum SavedLayout {
    Pane(String),
    Split(SavedSplit),
}
#[derive(Deserialize)]
#[serde(untagged)]
enum SavedSplit {
    Children {
        direction: MuxPaneSplitDirection,
        weights: Vec<f32>,
        children: Vec<MuxPaneLayout>,
    },
    Binary {
        direction: MuxPaneSplitDirection,
        ratio_millis: u16,
        first: Box<MuxPaneLayout>,
        second: Box<MuxPaneLayout>,
    },
}

impl<'de> Deserialize<'de> for MuxPaneLayout {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let layout = match SavedLayout::deserialize(deserializer)? {
            SavedLayout::Pane(id) => Self::Pane(id),
            SavedLayout::Split(SavedSplit::Children {
                direction,
                weights,
                children,
            }) => Self::Split {
                direction,
                weights,
                children,
            },
            SavedLayout::Split(SavedSplit::Binary {
                direction,
                ratio_millis,
                first,
                second,
            }) => {
                if !(1..=999).contains(&ratio_millis) {
                    return Err(serde::de::Error::custom("invalid split ratio"));
                }
                let ratio = f32::from(ratio_millis) / 1000.;
                Self::Split {
                    direction,
                    weights: vec![ratio, 1. - ratio],
                    children: vec![*first, *second],
                }
            }
        };
        normalize(layout)
            .ok_or_else(|| serde::de::Error::custom("invalid split weights or children"))
    }
}

/// The same post-order collapse and scaled same-axis splice as GPUI Kit's Dock.
fn normalize(node: MuxPaneLayout) -> Option<MuxPaneLayout> {
    let MuxPaneLayout::Split {
        direction,
        weights,
        children,
    } = node
    else {
        return Some(node);
    };
    if children.is_empty()
        || children.len() != weights.len()
        || weights
            .iter()
            .any(|weight| !weight.is_finite() || *weight <= 0.)
    {
        return None;
    }
    let total: f32 = weights.iter().sum();
    if !total.is_finite() {
        return None;
    }
    let mut flat_children = Vec::new();
    let mut flat_weights = Vec::new();
    for (child, weight) in children.into_iter().zip(weights) {
        let weight = weight / total;
        match normalize(child)? {
            MuxPaneLayout::Split {
                direction: inner,
                weights,
                children,
            } if inner == direction => {
                flat_weights.extend(weights.into_iter().map(|inner| weight * inner));
                flat_children.extend(children);
            }
            child => {
                flat_children.push(child);
                flat_weights.push(weight);
            }
        }
    }
    if flat_children.len() == 1 {
        return flat_children.pop();
    }
    if flat_weights.iter().any(|weight| *weight <= 0.) {
        return None;
    }
    Some(MuxPaneLayout::Split {
        direction,
        weights: flat_weights,
        children: flat_children,
    })
}

#[derive(Clone, Debug, PartialEq)]
pub struct PaneLayout {
    root: MuxPaneLayout,
    focused: PaneId,
}

impl PaneLayout {
    #[must_use]
    pub fn single(pane: PaneId) -> Self {
        Self {
            root: MuxPaneLayout::Pane(pane.clone()),
            focused: pane,
        }
    }

    #[must_use]
    pub fn from_mux_layout(layout: &MuxPaneLayout) -> Option<Self> {
        let root = normalize(layout.clone())?;
        let focused = first_leaf(&root)?.to_owned();
        let layout = Self { root, focused };
        let panes = layout.panes();
        let unique: std::collections::HashSet<_> = panes.iter().collect();
        (unique.len() == panes.len()).then_some(layout)
    }

    #[must_use]
    pub fn snapshot(&self) -> MuxPaneLayout {
        self.root.clone()
    }
    #[must_use]
    pub fn focused(&self) -> &str {
        &self.focused
    }
    #[must_use]
    pub const fn is_single(&self) -> bool {
        matches!(self.root, MuxPaneLayout::Pane(_))
    }
    #[must_use]
    pub fn contains(&self, pane: &str) -> bool {
        contains(&self.root, pane)
    }
    #[must_use]
    pub fn panes(&self) -> Vec<PaneId> {
        fn collect(node: &MuxPaneLayout, out: &mut Vec<PaneId>) {
            match node {
                MuxPaneLayout::Pane(id) => out.push(id.clone()),
                MuxPaneLayout::Split { children, .. } => {
                    for child in children {
                        collect(child, out);
                    }
                }
            }
        }
        let mut out = Vec::new();
        collect(&self.root, &mut out);
        out
    }
    pub fn set_focus(&mut self, pane: &str) -> bool {
        if !self.contains(pane) {
            return false;
        }
        pane.clone_into(&mut self.focused);
        true
    }
    pub fn split_focused(&mut self, pane: PaneId, direction: SplitDirection) {
        let direction = match direction {
            SplitDirection::Right => Direction::Right,
            SplitDirection::Down => Direction::Down,
        };
        let focused = self.focused.clone();
        self.insert_beside(pane, &focused, direction);
    }
    pub fn merge(&mut self, other: Self) -> bool {
        if other.panes().iter().any(|pane| self.contains(pane)) {
            return false;
        }
        let Some(root) = normalize(MuxPaneLayout::Split {
            direction: MuxPaneSplitDirection::Right,
            weights: vec![0.5, 0.5],
            children: vec![self.root.clone(), other.root],
        }) else {
            return false;
        };
        self.root = root;
        self.focused = other.focused;
        true
    }
    pub fn swap(&mut self, first: &str, second: &str) -> bool {
        if first == second || !self.contains(first) || !self.contains(second) {
            return false;
        }
        swap_leaves(&mut self.root, first, second);
        true
    }
    pub fn replace(&mut self, pane: &str, replacement: PaneId) -> bool {
        if !self.contains(pane) || self.contains(&replacement) {
            return false;
        }
        swap_leaves(&mut self.root, pane, &replacement);
        if self.focused == pane {
            self.focused = replacement;
        }
        true
    }
    pub fn insert_beside(&mut self, pane: PaneId, target: &str, direction: Direction) -> bool {
        if self.contains(&pane) || !self.contains(target) {
            return false;
        }
        let mut root = self.root.clone();
        insert_leaf(&mut root, &pane, target, direction);
        let Some(root) = normalize(root) else {
            return false;
        };
        self.root = root;
        self.focused = pane;
        true
    }
    pub fn move_beside(&mut self, pane: &str, target: &str, direction: Direction) -> bool {
        if pane == target || !self.contains(pane) || !self.contains(target) {
            return false;
        }
        let mut candidate = self.clone();
        if !candidate.remove(pane) || !candidate.insert_beside(pane.to_owned(), target, direction) {
            return false;
        }
        *self = candidate;
        true
    }
    pub fn remove(&mut self, pane: &str) -> bool {
        if self.is_single() || !self.contains(pane) {
            return false;
        }
        let mut root = self.root.clone();
        prune_leaf(&mut root, pane);
        let Some(root) = normalize(root) else {
            return false;
        };
        self.root = root;
        if !self.contains(&self.focused)
            && let Some(focused) = first_leaf(&self.root)
        {
            self.focused = focused.to_owned();
        }
        true
    }
    pub fn reconcile(&mut self, ids: &[PaneId]) {
        self.reconcile_with_new_pane_direction(ids, SplitDirection::Right);
    }
    pub fn reconcile_with_new_pane_direction(&mut self, ids: &[PaneId], direction: SplitDirection) {
        if let Some(first) = ids.first() {
            if !self.panes().iter().any(|pane| ids.contains(pane)) {
                *self = Self::single(first.clone());
            }
            for pane in self.panes() {
                if !ids.contains(&pane) {
                    self.remove(&pane);
                }
            }
            for pane in ids {
                if !self.contains(pane) {
                    self.split_focused(pane.clone(), direction);
                }
            }
        }
    }

    /// Accept weights only for the current structure; delayed drags cannot undo a topology edit.
    pub fn resize(&mut self, proposed: &MuxPaneLayout) -> bool {
        let Some(proposed) = Self::from_mux_layout(proposed) else {
            return false;
        };
        if !same_structure(&self.root, &proposed.root) {
            return false;
        }
        self.root = proposed.root;
        true
    }

    /// Server navigation geometry. UI geometry comes exclusively from Dock.
    #[must_use]
    pub fn rects(&self, area: SurfaceRect, gap: f32) -> Vec<(PaneId, SurfaceRect)> {
        fn visit(
            node: &MuxPaneLayout,
            area: SurfaceRect,
            gap: f32,
            out: &mut Vec<(PaneId, SurfaceRect)>,
        ) {
            match node {
                MuxPaneLayout::Pane(id) => out.push((id.clone(), area)),
                MuxPaneLayout::Split {
                    direction,
                    children,
                    weights,
                } => {
                    let horizontal = *direction == MuxPaneSplitDirection::Right;
                    let gaps = num_traits::ToPrimitive::to_f32(&children.len().saturating_sub(1))
                        .unwrap_or(0.);
                    let extent = if horizontal {
                        area.width()
                    } else {
                        area.height()
                    };
                    let usable = gap.mul_add(-gaps, extent).max(0.);
                    let mut offset = 0.;
                    for (child, weight) in children.iter().zip(weights) {
                        let size = usable * weight;
                        let rect = if horizontal {
                            SurfaceRect::from_min_size(
                                area.min_x + offset,
                                area.min_y,
                                size,
                                area.height(),
                            )
                        } else {
                            SurfaceRect::from_min_size(
                                area.min_x,
                                area.min_y + offset,
                                area.width(),
                                size,
                            )
                        };
                        visit(child, rect, gap, out);
                        offset += size + gap;
                    }
                }
            }
        }
        let mut out = Vec::new();
        visit(&self.root, area, gap, &mut out);
        out
    }

    #[must_use]
    pub fn neighbor(
        &self,
        from: &str,
        direction: Direction,
        area: SurfaceRect,
        gap: f32,
    ) -> Option<PaneId> {
        let rects = self.rects(area, gap);
        let origin = rects.iter().find(|(pane, _)| pane == from)?.1;
        rects
            .iter()
            .filter(|(pane, _)| pane != from)
            .filter_map(|(pane, rect)| {
                let (primary, overlap) = directional_gap(origin, *rect, direction)?;
                Some((f32::mul_add(overlap, -0.001, primary), pane))
            })
            .min_by(|a, b| a.0.total_cmp(&b.0))
            .map(|(_, pane)| pane.clone())
    }
}

fn first_leaf(node: &MuxPaneLayout) -> Option<&str> {
    match node {
        MuxPaneLayout::Pane(id) => Some(id),
        MuxPaneLayout::Split { children, .. } => first_leaf(children.first()?),
    }
}
fn contains(node: &MuxPaneLayout, pane: &str) -> bool {
    match node {
        MuxPaneLayout::Pane(id) => id == pane,
        MuxPaneLayout::Split { children, .. } => children.iter().any(|child| contains(child, pane)),
    }
}
fn swap_leaves(node: &mut MuxPaneLayout, first: &str, second: &str) {
    match node {
        MuxPaneLayout::Pane(id) if id == first => second.clone_into(id),
        MuxPaneLayout::Pane(id) if id == second => first.clone_into(id),
        MuxPaneLayout::Pane(_) => {}
        MuxPaneLayout::Split { children, .. } => {
            for child in children {
                swap_leaves(child, first, second);
            }
        }
    }
}
fn same_structure(a: &MuxPaneLayout, b: &MuxPaneLayout) -> bool {
    match (a, b) {
        (MuxPaneLayout::Pane(a), MuxPaneLayout::Pane(b)) => a == b,
        (
            MuxPaneLayout::Split {
                direction: a,
                children: ac,
                ..
            },
            MuxPaneLayout::Split {
                direction: b,
                children: bc,
                ..
            },
        ) => a == b && ac.len() == bc.len() && ac.iter().zip(bc).all(|(a, b)| same_structure(a, b)),
        _ => false,
    }
}

/// For a candidate rect relative to `origin` in `direction`, return `(distance_along_axis,
/// perpendicular_overlap)` when the candidate lies on the correct side with overlap, else `None`.
fn directional_gap(
    origin: SurfaceRect,
    candidate: SurfaceRect,
    direction: Direction,
) -> Option<(f32, f32)> {
    let origin_center_x = f32::midpoint(origin.min_x, origin.max_x);
    let origin_center_y = f32::midpoint(origin.min_y, origin.max_y);
    let candidate_center_x = f32::midpoint(candidate.min_x, candidate.max_x);
    let candidate_center_y = f32::midpoint(candidate.min_y, candidate.max_y);
    let (forward, primary, overlap) = match direction {
        Direction::Right => (
            candidate_center_x > origin_center_x,
            candidate.min_x - origin.max_x,
            vertical_overlap(origin, candidate),
        ),
        Direction::Left => (
            candidate_center_x < origin_center_x,
            origin.min_x - candidate.max_x,
            vertical_overlap(origin, candidate),
        ),
        Direction::Down => (
            candidate_center_y > origin_center_y,
            candidate.min_y - origin.max_y,
            horizontal_overlap(origin, candidate),
        ),
        Direction::Up => (
            candidate_center_y < origin_center_y,
            origin.min_y - candidate.max_y,
            horizontal_overlap(origin, candidate),
        ),
    };
    (forward && overlap > 0.0).then_some((primary, overlap))
}

fn vertical_overlap(a: SurfaceRect, b: SurfaceRect) -> f32 {
    (a.max_y.min(b.max_y) - a.min_y.max(b.min_y)).max(0.0)
}

fn horizontal_overlap(a: SurfaceRect, b: SurfaceRect) -> f32 {
    (a.max_x.min(b.max_x) - a.min_x.max(b.min_x)).max(0.0)
}

fn insert_leaf(node: &mut MuxPaneLayout, pane: &str, target: &str, direction: Direction) {
    match node {
        MuxPaneLayout::Pane(id) if id == target => {
            let mut children = vec![node.clone(), MuxPaneLayout::Pane(pane.to_owned())];
            if matches!(direction, Direction::Left | Direction::Up) {
                children.reverse();
            }
            *node = MuxPaneLayout::Split {
                direction: match direction {
                    Direction::Left | Direction::Right => MuxPaneSplitDirection::Right,
                    Direction::Up | Direction::Down => MuxPaneSplitDirection::Down,
                },
                weights: vec![0.5, 0.5],
                children,
            };
        }
        MuxPaneLayout::Pane(_) => {}
        MuxPaneLayout::Split { children, .. } => {
            for child in children {
                insert_leaf(child, pane, target, direction);
            }
        }
    }
}

fn prune_leaf(node: &mut MuxPaneLayout, pane: &str) {
    if let MuxPaneLayout::Split {
        children, weights, ..
    } = node
    {
        if let Some(ix) = children
            .iter()
            .position(|child| matches!(child, MuxPaneLayout::Pane(id) if id == pane))
        {
            children.remove(ix);
            weights.remove(ix);
        } else {
            for child in children {
                prune_leaf(child, pane);
            }
        }
    }
}
