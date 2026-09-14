use bootty_mux::pane_layout::{Direction, PaneId, PaneLayout, SplitDirection};
use bootty_mux::snapshot::{MuxPaneLayout, MuxPaneSplitDirection};
use bootty_terminal::geometry::{SurfacePoint, SurfaceRect};
use pretty_assertions::assert_eq;
use proptest::prelude::*;

fn area() -> SurfaceRect {
    SurfaceRect::from_min_size(0.0, 0.0, 100.0, 80.0)
}

fn rect_for<'a>(rects: &'a [(PaneId, SurfaceRect)], pane: &str) -> Option<&'a SurfaceRect> {
    rects
        .iter()
        .find_map(|(id, rect)| (id == pane).then_some(rect))
}

fn approx(actual: f32, expected: f32) {
    assert!((actual - expected).abs() < 0.01, "{actual} != {expected}");
}

#[derive(Clone, Copy, Debug, proptest_derive::Arbitrary)]
struct SplitLayoutInput {
    #[proptest(strategy = "10u16..2_000")]
    width: u16,
    #[proptest(strategy = "10u16..2_000")]
    height: u16,
    #[proptest(strategy = "0u8..=25")]
    gap_percent: u8,
    horizontal: bool,
}

#[test]
fn dividers_keep_their_split_direction_and_tree_path() {
    let mut layout = PaneLayout::single("a".to_owned());
    layout.split_focused("b".to_owned(), SplitDirection::Right);
    layout.split_focused("c".to_owned(), SplitDirection::Down);

    let dividers = layout.dividers(area(), 4.0);
    assert_eq!(dividers.len(), 2);
    assert_eq!(dividers[0].path, Vec::<u8>::new());
    assert_eq!(dividers[0].direction, SplitDirection::Right);
    assert_eq!(dividers[1].path, vec![1]);
    assert_eq!(dividers[1].direction, SplitDirection::Down);
}

#[test]
fn reconciliation_removes_closed_panes_and_adopts_new_panes() {
    let mut layout = PaneLayout::single("a".to_owned());
    layout.split_focused("b".to_owned(), SplitDirection::Right);
    layout.split_focused("c".to_owned(), SplitDirection::Down);

    layout.reconcile(&["a".to_owned(), "c".to_owned(), "d".to_owned()]);

    let mut panes = layout.panes();
    panes.sort();
    assert_eq!(panes, vec!["a".to_owned(), "c".to_owned(), "d".to_owned()]);
    assert!(layout.contains("d"));
    assert!(!layout.contains("b"));

    let before = layout.clone();
    layout.reconcile(&["a".to_owned(), "c".to_owned(), "d".to_owned()]);
    assert_eq!(layout, before);
}

#[test]
fn reconciliation_uses_the_requested_direction_for_an_async_pane() {
    let mut layout = PaneLayout::single("a".to_owned());

    layout
        .reconcile_with_new_pane_direction(&["a".to_owned(), "b".to_owned()], SplitDirection::Down);

    let dividers = layout.dividers(area(), 4.0);
    assert_eq!(dividers.len(), 1);
    assert_eq!(dividers[0].direction, SplitDirection::Down);
    assert_eq!(layout.focused(), "b");
}

#[test]
fn backend_layout_restores_split_orientation_and_ratio() {
    let layout = PaneLayout::from_mux_layout(&MuxPaneLayout::Split {
        direction: MuxPaneSplitDirection::Down,
        ratio_millis: 250,
        first: Box::new(MuxPaneLayout::Pane("a".to_owned())),
        second: Box::new(MuxPaneLayout::Pane("b".to_owned())),
    })
    .expect("mux layout should convert");

    let dividers = layout.dividers(area(), 0.0);
    assert_eq!(dividers.len(), 1);
    assert_eq!(dividers[0].direction, SplitDirection::Down);
    let rects = layout.rects(area(), 0.0);
    approx(rect_for(&rects, "a").expect("pane present").height(), 20.0);
    approx(rect_for(&rects, "b").expect("pane present").height(), 60.0);
}

#[test]
fn terminal_window_size_includes_internal_split_borders() {
    let mut right = PaneLayout::single("a".to_owned());
    right.split_focused("b".to_owned(), SplitDirection::Right);

    assert_eq!(
        right.terminal_window_size(|pane| match pane {
            "a" | "b" => Some((58, 40)),
            _ => None,
        }),
        Some((117, 40))
    );

    let mut nested = PaneLayout::single("a".to_owned());
    nested.split_focused("b".to_owned(), SplitDirection::Right);
    nested.split_focused("c".to_owned(), SplitDirection::Down);

    assert_eq!(
        nested.terminal_window_size(|pane| match pane {
            "a" => Some((58, 39)),
            "b" | "c" => Some((58, 19)),
            _ => None,
        }),
        Some((117, 39))
    );
}

proptest! {
    /// Property: a split partitions the available axis exactly once, accounting for its gap.
    #[test]
    fn split_rectangles_conserve_available_space(input in any::<SplitLayoutInput>()) {
        let SplitLayoutInput { width, height, gap_percent, horizontal } = input;
        let width = f32::from(width);
        let height = f32::from(height);
        let area = SurfaceRect::from_min_size(0.0, 0.0, width, height);
        let gap = f32::from(gap_percent) / 100.0 * if horizontal { width } else { height };
        let single = PaneLayout::single("a".to_owned());
        prop_assert_eq!(*rect_for(&single.rects(area, gap), "a").expect("pane present"), area);
        prop_assert!(single.dividers(area, gap).is_empty() && single.is_single());
        let direction = if horizontal { SplitDirection::Right } else { SplitDirection::Down };
        let mut layout = PaneLayout::single("a".to_owned());
        layout.split_focused("b".to_owned(), direction);
        prop_assert_eq!(layout.focused(), "b");
        prop_assert_eq!(layout.panes(), vec!["a".to_owned(), "b".to_owned()]);
        let rects = layout.rects(area, gap);
        let divider = layout.dividers(area, gap).remove(0);
        let center = SurfacePoint { x: width * 0.5, y: height * 0.5 };
        prop_assert!((divider.ratio_at(center, 0.0) - 0.5).abs() < 0.01);
        let first = rect_for(&rects, "a").expect("pane present");
        let second = rect_for(&rects, "b").expect("pane present");
        let occupied = if horizontal {
            first.width() + gap + second.width()
        } else {
            first.height() + gap + second.height()
        };

        prop_assert!((occupied - if horizontal { width } else { height }).abs() < 0.01,
            "first={first:?}, second={second:?}, gap={gap}, area={area:?}");
        let separated = if horizontal {
            first.max_x <= second.min_x
        } else {
            first.max_y <= second.min_y
        };
        prop_assert!(separated, "split panes overlap: {first:?}, {second:?}");
        if horizontal {
            prop_assert_eq!(first.height().to_bits(), height.to_bits());
            prop_assert_eq!(second.height().to_bits(), height.to_bits());
        } else {
            prop_assert_eq!(first.width().to_bits(), width.to_bits());
            prop_assert_eq!(second.width().to_bits(), width.to_bits());
        }

        let (forward, backward, outside) = if horizontal {
            (Direction::Right, Direction::Left, Direction::Up)
        } else {
            (Direction::Down, Direction::Up, Direction::Left)
        };
        prop_assert_eq!(layout.neighbor("a", forward, area, gap), Some("b".to_owned()));
        prop_assert_eq!(layout.neighbor("b", backward, area, gap), Some("a".to_owned()));
        prop_assert_eq!(layout.neighbor("a", outside, area, gap), None);

        let mut nested = layout.clone();
        nested.split_focused("c".to_owned(), if horizontal { SplitDirection::Down } else { SplitDirection::Right });
        prop_assert_eq!(nested.panes(), vec!["a".to_owned(), "b".to_owned(), "c".to_owned()]);
        let nested_rects = nested.rects(area, 0.0);
        let primary = |rect: &SurfaceRect| if horizontal { rect.width() / width } else { rect.height() / height };
        let secondary = |rect: &SurfaceRect| if horizontal { rect.height() / height } else { rect.width() / width };
        prop_assert!((primary(rect_for(&nested_rects, "a").expect("pane present")) - 0.5).abs() < 0.01);
        prop_assert!((primary(rect_for(&nested_rects, "b").expect("pane present")) - 0.5).abs() < 0.01);
        prop_assert!((secondary(rect_for(&nested_rects, "b").expect("pane present")) - 0.5).abs() < 0.01);
        nested.reconcile(&["a".to_owned(), "b".to_owned()]);
        let reconciled = nested.rects(area, 0.0);
        prop_assert!((secondary(rect_for(&reconciled, "b").expect("pane present")) - 1.0).abs() < 0.01);

        layout.set_ratio_at(&[], 0.99, 0.1, 0.2);
        let clamped = layout.rects(area, 0.0);
        let first_fraction = if horizontal {
            rect_for(&clamped, "a").expect("pane present").width() / width
        } else {
            rect_for(&clamped, "a").expect("pane present").height() / height
        };
        prop_assert!((first_fraction - 0.8).abs() < 0.01, "clamped ratio={first_fraction}");

        let mut collapsed = layout;
        prop_assert!(collapsed.remove("b"));
        prop_assert_eq!(collapsed.panes(), vec!["a".to_owned()]);
        prop_assert_eq!(collapsed.focused(), "a");
        prop_assert_eq!(*rect_for(&collapsed.rects(area, gap), "a").expect("pane present"), area);
        prop_assert!(!collapsed.remove("a"));
    }
}

proptest! {
    #[test]
    fn pane_moves_preserve_identities_and_swaps_preserve_geometry(count in 2usize..20, first in 0usize..20, second in 0usize..20, vertical in any::<bool>(), before in any::<bool>()) {
        let ids=(0..count).map(|index|format!("pane-{index}")).collect::<Vec<_>>();
        let mut layout=PaneLayout::single(ids[0].clone());
        for (index,pane) in ids.iter().enumerate().skip(1) { layout.split_focused(pane.clone(),if index%2==0{SplitDirection::Down}else{SplitDirection::Right}); }
        let source=&ids[first.checked_rem(count).expect("nonzero count")];let target=&ids[second.checked_rem(count).expect("nonzero count")];
        let original=layout.clone();
        if source==target {
            prop_assert!(!layout.move_beside(source,target,Direction::Left));
            prop_assert_eq!(layout,original); return Ok(());
        }
        let rects=layout.rects(area(),0.0);
        prop_assert!(layout.swap(source,target));
        let swapped=layout.rects(area(),0.0);
        prop_assert_eq!(rect_for(&swapped,source).expect("pane present"),rect_for(&rects,target).expect("pane present"));
        prop_assert_eq!(rect_for(&swapped,target).expect("pane present"),rect_for(&rects,source).expect("pane present"));
        prop_assert_eq!(layout.focused(),original.focused());
        prop_assert!(layout.swap(source,target));prop_assert_eq!(&layout,&original);
        let direction=match (vertical,before){(true,true)=>Direction::Up,(true,false)=>Direction::Down,(false,true)=>Direction::Left,(false,false)=>Direction::Right};
        prop_assert!(layout.move_beside(source,target,direction));
        let mut actual=layout.panes();actual.sort();let mut expected=ids.clone();expected.sort();
        prop_assert_eq!(actual,expected);prop_assert_eq!(layout.focused(),source);
        let rects=layout.rects(area(),0.0);let source_rect=rect_for(&rects,source).expect("pane present");let target_rect=rect_for(&rects,target).expect("pane present");
        match direction {Direction::Left=>prop_assert!(source_rect.max_x<=target_rect.min_x),Direction::Right=>prop_assert!(target_rect.max_x<=source_rect.min_x),Direction::Up=>prop_assert!(source_rect.max_y<=target_rect.min_y),Direction::Down=>prop_assert!(target_rect.max_y<=source_rect.min_y)}
        let saved=layout.clone();prop_assert!(!layout.move_beside(source,"missing",direction));prop_assert!(!layout.insert_beside(source.clone(),target,direction));prop_assert_eq!(layout,saved);
    }
}

proptest! {
    #[test]
    fn merging_preserves_both_trees_and_rejects_overlapping_identities(ratio in 0.1f32..0.9) {
        let mut left = PaneLayout::single("a".to_owned());
        left.split_focused("b".to_owned(), SplitDirection::Down);
        left.set_ratio_at(&[], ratio, 0.05, 0.05);
        let mut right = PaneLayout::single("c".to_owned());
        right.split_focused("d".to_owned(), SplitDirection::Right);
        let duplicate = left.clone();
        prop_assert!(!left.merge(duplicate.clone())); prop_assert_eq!(&left, &duplicate);
        prop_assert!(left.merge(right));
        prop_assert_eq!(left.panes(), vec!["a", "b", "c", "d"]);
        prop_assert_eq!(left.focused(), "d");
        let rects = left.rects(area(), 0.);
        prop_assert!((rect_for(&rects,"a").expect("pane present").height() / area().height() - ratio).abs() < 0.001);
        prop_assert!(!left.replace("a", "c".to_owned()));
        prop_assert!(left.replace("a", "replacement".to_owned()));
        prop_assert_eq!(left.panes(), vec!["replacement", "b", "c", "d"]);
    }
}
