use super::frames as frame_inputs;

pub fn idle_frame(now: std::time::Instant) -> bootty_ui::FrameInputs {
    frame_inputs::frame(now, Vec::new())
}
