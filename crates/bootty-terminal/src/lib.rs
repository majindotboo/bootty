mod pty_backlog;
mod terminal_png_decoder;

pub mod benchmark_trace;
pub mod frame_source;
pub mod geometry;
pub mod latency;
pub mod perf;
pub mod scheduler;
pub mod selection;
pub mod shell_integration;
pub mod shell_lifecycle;
pub mod terminal;
pub mod terminal_capture;
pub mod terminal_engine;
pub mod terminal_frame;
pub mod terminal_image;
pub mod terminal_input;
pub mod terminal_input_model;
pub mod terminal_launch;
pub mod terminal_links;
pub mod terminal_palette;
pub mod terminal_search;
pub mod terminal_session;
pub mod terminal_side_effect;
pub mod terminfo;

pub use benchmark_trace::{BenchmarkTrace, TraceValue};
pub use pty_backlog::{
    OutputBacklog, PtyBacklog, drain_output_backlog, drain_output_backlog_with_limits,
    drain_pty_backlog,
};
pub use terminal_session::{
    DrainStats, SessionLaunchConfig, TerminalSession, TerminalSessionConfig,
};

pub use libghostty_vt::style::RgbColor;
pub use terminal_input::TerminalInputCommand;

pub mod shell_prompt;

pub mod clipboard_write;
