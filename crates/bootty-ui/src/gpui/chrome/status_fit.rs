//! Fit whole status controls into the header; omitted controls have no hitboxes.
use num_traits::ToPrimitive as _;

use gpui_kit::{
    AnyElement, App, AvailableSpace, Bounds, Element, ElementId, GlobalElementId,
    InspectorElementId, IntoElement, LayoutId, Pixels, Size, Style, Window, point, px, size,
};

pub(super) struct StatusFit {
    pub id: ElementId,
    pub items: Vec<AnyElement>,
    pub height: Pixels,
    pub gap: Pixels,
    pub partition: Option<(Pixels, bool)>,
}
impl IntoElement for StatusFit {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}
impl Element for StatusFit {
    type RequestLayoutState = Vec<(Size<Pixels>, bool)>;
    type PrepaintState = Vec<usize>;
    fn id(&self) -> Option<ElementId> {
        Some(self.id.clone())
    }
    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }
    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let mut measured: Vec<_> = self
            .items
            .iter_mut()
            .map(|item| {
                (
                    item.layout_as_root(
                        Size {
                            width: AvailableSpace::MaxContent,
                            height: AvailableSpace::Definite(self.height),
                        },
                        window,
                        cx,
                    ),
                    true,
                )
            })
            .collect();
        // Report the controls' natural width, not the entire header width: the mux tabs
        // need the remaining space. Prepaint still omits whole controls when constrained.
        if let Some((remaining, in_dock)) = self.partition {
            let mut remaining = f32::from(remaining);
            for (item, eligible) in measured.iter_mut().rev() {
                let fits = f32::from(item.width) <= remaining;
                if fits {
                    remaining -= f32::from(item.width) + f32::from(self.gap);
                }
                *eligible = fits == in_dock;
            }
        }
        let count = measured.iter().filter(|(_, eligible)| *eligible).count();
        let item_width = measured
            .iter()
            .filter(|(_, eligible)| *eligible)
            .map(|(item, _)| f32::from(item.width))
            .sum::<f32>();
        let width = px(f32::from(self.gap).mul_add(
            count.saturating_sub(1).to_f32().unwrap_or(f32::MAX),
            item_width,
        ));
        let style = Style {
            size: size(width.into(), self.height.into()),
            min_size: size(px(0.0).into(), self.height.into()),
            ..Default::default()
        };
        (window.request_layout(style, [], cx), measured)
    }
    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        measured: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Vec<usize> {
        let mut right = f32::from(bounds.right());
        let mut visible = Vec::new();
        for (ix, (item, (measured, eligible))) in
            self.items.iter_mut().zip(measured.iter()).enumerate().rev()
        {
            if !*eligible {
                continue;
            }
            if f32::from(measured.width) <= right - f32::from(bounds.left()) {
                right -= f32::from(measured.width);
                item.prepaint_at(
                    point(
                        px(right),
                        px((f32::from(bounds.size.height) - f32::from(measured.height))
                            .mul_add(0.5, f32::from(bounds.top()))),
                    ),
                    window,
                    cx,
                );
                visible.push(ix);
                right -= f32::from(self.gap);
            }
        }
        visible
    }
    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        visible: &mut Vec<usize>,
        window: &mut Window,
        cx: &mut App,
    ) {
        for ix in visible.iter().rev() {
            if let Some(item) = self.items.get_mut(*ix) {
                item.paint(window, cx);
            }
        }
    }
}
