//! Parts of a conversation that redraw on their own: the transcript, reused from the last frame
//! until the conversation changes, and the working row, which the motion clock redraws without
//! laying the transcript out again.
use super::{
    Context, Entity, IntoElement, NativeAgentSessionView, ParentElement, Render, Styled,
    Subscription, Window, div,
};

/// The transcript, drawn as a cached view. It redraws when the conversation notifies.
pub(super) struct TranscriptPane {
    session: gpui_kit::WeakEntity<NativeAgentSessionView>,
    _follow: Subscription,
}

/// The working row: progress, the working clock and running subagents.
pub(super) struct ActivityPane {
    session: gpui_kit::WeakEntity<NativeAgentSessionView>,
}

impl TranscriptPane {
    pub(super) fn new(session: &Entity<NativeAgentSessionView>, cx: &mut Context<Self>) -> Self {
        Self {
            session: session.downgrade(),
            _follow: cx.observe(session, |_, _, cx| cx.notify()),
        }
    }

    /// The layout the cached transcript takes, matching its own root.
    pub(super) fn style() -> gpui_kit::StyleRefinement {
        gpui_kit::StyleRefinement::default()
            .relative()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .flex()
    }
}

impl ActivityPane {
    pub(super) fn new(session: &Entity<NativeAgentSessionView>) -> Self {
        Self {
            session: session.downgrade(),
        }
    }
}

impl Render for TranscriptPane {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let drawn = self.session.update(cx, |session, cx| {
            session.step_glide(window, cx);
            session.render_transcript(window, cx).into_any_element()
        });
        // A cached view lays out as a root, so it fills the place the conversation gave it.
        div().size_full().flex().children(drawn.ok())
    }
}

impl Render for ActivityPane {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.session
            .update(cx, |session, cx| {
                let busy = session.busy();
                session.render_activity(busy, window, cx).into_any_element()
            })
            .unwrap_or_else(|_| div().into_any_element())
    }
}
