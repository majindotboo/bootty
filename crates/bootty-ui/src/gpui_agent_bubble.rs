//! Prompt sizing uses the rich-text layout that will actually be painted.
use gpui_kit::{
    AnyElement, App, AvailableSpace, Bounds, Element, ElementId, FlexDirection, GlobalElementId,
    InspectorElementId, IntoElement, LayoutId, Length, Pixels, Size, Style, Window, px, relative,
    size,
};
use num_traits::ToPrimitive as _;
use std::ops::{Div as _, Mul as _};

pub(super) struct PromptBubble {
    pub id: ElementId,
    pub content: AnyElement,
}

impl IntoElement for PromptBubble {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

impl PromptBubble {
    fn measure(
        &mut self,
        width: AvailableSpace,
        window: &mut Window,
        cx: &mut App,
    ) -> Size<Pixels> {
        self.content
            .layout_as_root(size(width, AvailableSpace::MaxContent), window, cx)
    }
}

impl Element for PromptBubble {
    type RequestLayoutState = LayoutId;
    type PrepaintState = ();

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
    ) -> (LayoutId, LayoutId) {
        let child = self.content.request_layout(window, cx);
        let maximum = window.viewport_size().width;
        let bounded = self.measure(AvailableSpace::Definite(maximum), window, cx);
        let intrinsic = self.measure(AvailableSpace::MaxContent, window, cx);
        let preferred = if intrinsic.height <= bounded.height {
            intrinsic.width
        } else {
            // A flexible list item can report only its marker as its intrinsic
            // width. Find the width its actual text needs without adding lines.
            let scale = window.scale_factor();
            let mut low = 0_u32;
            let mut high = f32::from(maximum)
                .mul(scale)
                .ceil()
                .to_u32()
                .unwrap_or(u32::MAX);
            while high.saturating_sub(low) > 1 {
                let middle = low.midpoint(high);
                if self
                    .measure(
                        AvailableSpace::Definite(px(middle
                            .to_f32()
                            .unwrap_or(f32::MAX)
                            .div(scale))),
                        window,
                        cx,
                    )
                    .height
                    > bounded.height
                {
                    low = middle;
                } else {
                    high = middle;
                }
            }
            px(high.to_f32().unwrap_or(f32::MAX).div(scale))
        };
        let style = Style {
            flex_direction: FlexDirection::Column,
            size: size(preferred.into(), Length::Auto),
            max_size: size(relative(0.8).into(), Length::Auto),
            min_size: size(px(0.).into(), Length::Auto),
            ..Default::default()
        };
        (window.request_layout(style, [child], cx), child)
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        child: &mut LayoutId,
        window: &mut Window,
        cx: &mut App,
    ) {
        // Refresh the measured root's bounds at the width its parent actually assigned.
        window.compute_layout(
            *child,
            size(
                AvailableSpace::Definite(bounds.size.width),
                AvailableSpace::MaxContent,
            ),
            cx,
        );
        self.content.prepaint(window, cx);
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut LayoutId,
        (): &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        self.content.paint(window, cx);
    }
}
