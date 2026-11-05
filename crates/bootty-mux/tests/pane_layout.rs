use bootty_mux::{
    pane_layout::{Direction, PaneLayout, SplitDirection},
    snapshot::{MuxPaneLayout, MuxPaneSplitDirection},
};
use bootty_terminal::geometry::SurfaceRect;
use pretty_assertions::assert_eq;
use proptest::prelude::*;
use rstest::rstest;

fn normalized(layout: &MuxPaneLayout, parent: Option<&MuxPaneSplitDirection>) -> bool {
    match layout {
        MuxPaneLayout::Pane(_) => true,
        MuxPaneLayout::Split {
            direction,
            children,
            weights,
        } => {
            parent != Some(direction)
                && children.len() >= 2
                && children.len() == weights.len()
                && weights
                    .iter()
                    .all(|weight| weight.is_finite() && *weight > 0.)
                && (weights.iter().sum::<f32>() - 1.).abs() < 0.000_01
                && children
                    .iter()
                    .all(|child| normalized(child, Some(direction)))
        }
    }
}
#[derive(Clone, Debug, proptest_derive::Arbitrary)]
enum Mutation {
    Split(bool),
    Close(u8),
    Move(u8, u8, bool),
    Swap(u8, u8),
    Extract(u8),
    Merge(bool),
    Resize(u16),
    Reconcile(u8),
    Focus(u8),
}
proptest! {
    #[test]
    fn every_mutation_keeps_a_canonical_tree(operations in prop::collection::vec(any::<Mutation>(), 0..100)) {
        let mut layout = PaneLayout::single("pane-0".into());
        let mut next = 1_u32;
        for op in operations {
            let panes = layout.panes();
            let at = |ix: u8| panes[usize::from(ix).checked_rem(panes.len()).unwrap()].as_str();
            match op {
                Mutation::Split(vertical) => { layout.split_focused(format!("pane-{next}"), if vertical { SplitDirection::Down } else { SplitDirection::Right }); next = next.saturating_add(1); }
                Mutation::Close(ix) | Mutation::Extract(ix) => { layout.remove(at(ix)); }
                Mutation::Move(a,b,vertical) => { layout.move_beside(at(a),at(b), if vertical { Direction::Up } else { Direction::Right }); }
                Mutation::Swap(a,b) => { layout.swap(at(a),at(b)); }
                Mutation::Merge(vertical) => {
                    let mut other = PaneLayout::single(format!("pane-{next}")); next = next.saturating_add(1);
                    other.split_focused(format!("pane-{next}"), if vertical { SplitDirection::Down } else { SplitDirection::Right }); next = next.saturating_add(1);
                    prop_assert!(layout.merge(other));
                }
                Mutation::Resize(seed) => {
                    let mut snapshot = layout.snapshot();
                    if let MuxPaneLayout::Split { weights, .. } = &mut snapshot {
                        for (ix,weight) in weights.iter_mut().enumerate() { *weight = f32::from(seed) + num_traits::ToPrimitive::to_f32(&ix).unwrap() + 1.; }
                    }
                    prop_assert!(layout.resize(&snapshot));
                }
                Mutation::Reconcile(ix) => { let mut live = panes.clone(); live.remove(usize::from(ix).checked_rem(live.len()).unwrap()); if !live.is_empty() { layout.reconcile(&live); } }
                Mutation::Focus(ix) => { prop_assert!(layout.set_focus(at(ix))); }
            }
            let snapshot = layout.snapshot();
            prop_assert!(normalized(&snapshot,None), "{snapshot:?}");
            let leaves = layout.panes(); let unique = leaves.iter().collect::<std::collections::HashSet<_>>();
            prop_assert_eq!(leaves.len(),unique.len()); prop_assert!(layout.contains(layout.focused()));
            let roundtrip: MuxPaneLayout = serde_json::from_str(&serde_json::to_string(&snapshot).unwrap()).unwrap();
            prop_assert!(normalized(&roundtrip,None));
            prop_assert_eq!(PaneLayout::from_mux_layout(&roundtrip).unwrap().panes(),leaves);
        }
    }
    #[test]
    fn swaps_preserve_geometry_and_focus(count in 2usize..15, a in any::<usize>(), b in any::<usize>()) {
        let mut layout = PaneLayout::single("0".into());
        for ix in 1..count { layout.split_focused(ix.to_string(), if ix%2 == 0 { SplitDirection::Down } else { SplitDirection::Right }); }
        let (a,b) = ((a.checked_rem(count).unwrap()).to_string(),(b.checked_rem(count).unwrap()).to_string());
        let area = SurfaceRect::from_min_size(0.,0.,1000.,800.);
        let before = layout.rects(area,0.); let focus = layout.focused().to_owned();
        prop_assert_eq!(layout.swap(&a,&b),a != b);
        let after = layout.rects(area,0.);
        prop_assert_eq!(layout.focused(),focus);
        for (id,rect) in before { let expected = if id==a { &b } else if id==b { &a } else { &id }; prop_assert_eq!(after.iter().find(|(id,_)| id==expected).unwrap().1,rect); }
    }
}
#[rstest]
fn resize_cannot_replace_topology_and_invalid_mutations_are_atomic() {
    let mut layout = PaneLayout::single("a".into());
    layout.split_focused("b".into(), SplitDirection::Right);
    let before = layout.clone();
    assert!(!layout.resize(&MuxPaneLayout::Pane("a".into())));
    assert!(!layout.move_beside("a", "missing", Direction::Left));
    assert!(!layout.insert_beside("a".into(), "b", Direction::Right));
    assert!(!layout.merge(before.clone()));
    assert_eq!(layout, before);
}
