//! Bake's native terminal preview: a pure draft editor and preview state,
//! a Ratatui renderer, and the terminal owner that runs them.

pub mod app;
pub mod editor;
pub mod render;
mod terminal;

pub use terminal::{PreviewExit, TerminalSession, run_preview};
