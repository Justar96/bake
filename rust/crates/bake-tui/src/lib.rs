//! Bake's native terminal preview: a pure draft editor and preview state,
//! the composer box, the header's activity line, a Ratatui renderer, and the
//! terminal owner that runs them.

pub mod activity;
pub mod app;
pub mod composer;
pub mod editor;
pub mod frame;
pub mod render;
mod terminal;

pub use terminal::{PreviewExit, TerminalSession, run_preview};
