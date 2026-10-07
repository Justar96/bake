//! Bake's native terminal preview: the terminal owner, input decoding, and
//! the event loop that drives the pure view in `bake-tui-view`.

mod input;
mod terminal;

pub use terminal::{PreviewExit, TerminalSession, run_preview};
