//! One header row fits optional status cells between the controls that must stay available.
use gpui_kit::{
    AnyElement, App, AvailableSpace, Bounds, Element, ElementId, GlobalElementId,
    InspectorElementId, IntoElement, LayoutId, Pixels, Rems, Size, Style, Window, point, px, size,
};
use num_traits::ToPrimitive as _;

pub(super) struct StatusFit {
    pub id: ElementId,
    pub leading: Vec<AnyElement>,
    pub items: Vec<AnyElement>,
    pub trailing: Vec<AnyElement>,
    pub height: Pixels,
    pub gap: Rems,
}
impl StatusFit {
    const fn required(&self, index: usize) -> bool {
        index < self.leading.len() || index >= self.leading.len().saturating_add(self.items.len())
    }
    fn width(&self, sizes: &[Size<Pixels>], required_only: bool, gap: f32) -> f32 {
        let mut width = 0.0;
        let mut count = 0_usize;
        for (ix, size) in sizes.iter().enumerate() {
            if !required_only || self.required(ix) {
                width += f32::from(size.width);
                count = count.saturating_add(1);
            }
        }
        gap.mul_add(count.saturating_sub(1).to_f32().unwrap_or(f32::MAX), width)
    }
}
impl IntoElement for StatusFit {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}
impl Element for StatusFit {
    type RequestLayoutState = Vec<Size<Pixels>>;
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
        let measured = self
            .leading
            .iter_mut()
            .chain(&mut self.items)
            .chain(&mut self.trailing)
            .map(|item| {
                item.layout_as_root(
                    Size {
                        width: AvailableSpace::MaxContent,
                        height: AvailableSpace::Definite(self.height),
                    },
                    window,
                    cx,
                )
            })
            .collect::<Vec<_>>();
        let gap = f32::from(self.gap.to_pixels(window.rem_size()));
        let style = Style {
            size: size(
                px(self.width(&measured, false, gap)).into(),
                self.height.into(),
            ),
            min_size: size(
                px(self.width(&measured, true, gap)).into(),
                self.height.into(),
            ),
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
        let gap = f32::from(self.gap.to_pixels(window.rem_size()));
        let mut visible = measured
            .iter()
            .enumerate()
            .filter(|(ix, _)| self.required(*ix))
            .map(|(ix, _)| ix)
            .collect::<Vec<_>>();
        let mut width = self.width(measured, true, gap);
        for (ix, size) in measured
            .iter()
            .enumerate()
            .rev()
            .filter(|(ix, _)| !self.required(*ix))
        {
            let added = f32::from(size.width) + if visible.is_empty() { 0.0 } else { gap };
            if width + added <= f32::from(bounds.size.width) {
                width += added;
                visible.push(ix);
            }
        }
        visible.sort_unstable();
        let mut x = f32::from(bounds.right()) - width;
        for ((ix, item), size) in self
            .leading
            .iter_mut()
            .chain(&mut self.items)
            .chain(&mut self.trailing)
            .enumerate()
            .zip(measured.iter())
        {
            if visible.binary_search(&ix).is_ok() {
                item.prepaint_at(
                    point(
                        px(x),
                        px((f32::from(bounds.size.height) - f32::from(size.height))
                            .mul_add(0.5, f32::from(bounds.top()))),
                    ),
                    window,
                    cx,
                );
                x += f32::from(size.width) + gap;
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
        for (ix, item) in self
            .leading
            .iter_mut()
            .chain(&mut self.items)
            .chain(&mut self.trailing)
            .enumerate()
        {
            if visible.binary_search(&ix).is_ok() {
                item.paint(window, cx);
            }
        }
    }
}
