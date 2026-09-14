mod ghostty_ffi_compat;
mod terminal_png_decoder;

pub mod terminal_engine;
pub mod terminal_frame;
pub mod terminal_image;
pub mod terminal_input_model;
pub mod terminal_palette;
pub mod terminal_side_effect;

pub mod geometry;

pub mod selection;

pub mod terminal {
    pub use crate::terminal_engine::{
        TERMINAL_BACKGROUND, TERMINAL_FOREGROUND, TerminalCursorConfig, TerminalCursorStyle,
        TerminalEngine, TerminalLiveConfig, TerminalSearchDirection, TerminalSelectionFormat,
    };
    pub use crate::terminal_frame::{
        CellStyle, CursorSnapshot, FrameColors, FrameScrollbar, FrameSelection, FrameStats,
        RenderCell, RenderFrame,
    };
    pub use crate::terminal_input_model::{
        KeyInput, KeyMods, MacosOptionAsAlt, MouseAction, MouseButton, MouseEncoderSize,
        MouseInput, TerminalKey,
    };
    pub use crate::terminal_session::{DrainStats, TerminalSession};
}

pub mod benchmark_trace;
pub mod frame_source;
pub mod latency;
pub mod perf;
mod pty_backlog;
pub mod scheduler;
pub mod terminal_launch;
pub mod terminal_session;
pub mod terminfo;

pub use benchmark_trace::{BenchmarkTrace, TraceValue};
pub use pty_backlog::{
    OutputBacklog, PtyBacklog, drain_output_backlog, drain_output_backlog_with_limits,
    drain_pty_backlog,
};
pub use terminal_session::{
    DrainStats, SessionLaunchConfig, TerminalSession, TerminalSessionConfig,
};
