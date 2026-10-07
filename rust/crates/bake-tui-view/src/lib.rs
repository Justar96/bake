//! Bake's terminal presentation: the draft editor, key bindings, presentation
//! state and its update function, and the Ratatui view of that state.
//!
//! Everything here is pure. Input arrives as [`state::Msg`] values and time as
//! [`state::Msg::Tick`]; [`state::update`] returns the effects the terminal
//! owner performs. This crate does not link a terminal backend, so it cannot
//! change terminal modes, read input, start threads, or read a clock.

pub mod activity;
pub mod composer;
mod copy;
pub mod diff;
pub mod editor;
pub mod frame;
pub mod keys;
pub mod layout;
pub mod mode;
pub mod render;
pub mod shell_output;
pub mod state;
pub mod status;
pub mod syntax;
pub mod transcript;
pub mod wheel;
