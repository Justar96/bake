//! Bake's native terminal preview: the terminal owner, input decoding, the
//! event loop that drives the pure view in `bake-tui-view`, and the runtime
//! port with its scripted fixture.

mod clipboard;
mod files;
mod fixture;
mod git;
mod inline;
mod input;
mod port;
mod terminal;

pub use terminal::{PreviewExit, ScreenMode, TerminalSession, run_preview, run_preview_mode};
