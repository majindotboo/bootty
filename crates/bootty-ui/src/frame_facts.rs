/// Host-neutral renderer facts consumed by diagnostics and repaint scheduling.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RendererMetrics {
    pub dirty_rows: usize,
    pub text_runs: usize,
    pub cursor_blinking: bool,
}
