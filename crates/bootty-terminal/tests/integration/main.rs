#![cfg(test)]

#[path = "../capture.rs"]
mod capture;

#[path = "../clipboard_write.rs"]
mod clipboard_write;

#[path = "../dirty_tracking.rs"]
mod dirty_tracking;

#[path = "../geometry.rs"]
mod geometry;

#[path = "../input_model.rs"]
mod input_model;

#[path = "../palette.rs"]
mod palette;

#[path = "../recovery.rs"]
mod recovery;

#[path = "../restored_history.rs"]
mod restored_history;

#[path = "../runtime_behavior.rs"]
mod runtime_behavior;

#[path = "../selection.rs"]
mod selection;

#[path = "../shell_integration.rs"]
mod shell_integration;

#[path = "../shell_lifecycle.rs"]
mod shell_lifecycle;

#[path = "../shell_prompt.rs"]
mod shell_prompt;

#[path = "../terminal_input.rs"]
mod terminal_input;

#[path = "../terminal_links.rs"]
mod terminal_links;

#[path = "../terminal_search.rs"]
mod terminal_search;

#[path = "../terminal_side_effect.rs"]
mod terminal_side_effect;

#[path = "../terminfo.rs"]
mod terminfo;

#[path = "../terminal_engine/main.rs"]
mod terminal_engine;

mod frame_wire;
