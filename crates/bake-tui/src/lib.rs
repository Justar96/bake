//! Bake's native terminal interface: the terminal owner, input decoding,
//! the event loop that drives the pure view in `bake-tui-view`, and the
//! runtime ports: the preview's scripted fixture and a live agent session
//! ([`live`]), and what a live session reports about itself to Herdr and to
//! the terminal (title, progress, and notifications).

mod clipboard;
mod files;
mod fixture;
mod git;
mod herdr;
mod indicators;
mod inline;
mod input;
mod lifecycle;
pub mod live;
mod port;
mod terminal;
// Read on Windows only; tested everywhere.
#[cfg_attr(not(windows), allow(dead_code))]
mod vt_parse;
#[cfg(windows)]
mod windows_input;

pub use bake_tui_view::transcript::{Banner, Notice, Pill};
pub use live::{LiveSession, UpdateCheck};
pub use terminal::{
    PreviewExit, ScreenMode, TerminalSession, run_live, run_preview, run_preview_mode,
};
