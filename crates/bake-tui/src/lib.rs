//! Bake's native terminal interface: the terminal owner, input decoding,
//! the event loop that drives the pure view in `bake-tui-view`, and the
//! runtime ports: the preview's scripted fixture and a live agent session
//! ([`live`]).

mod clipboard;
mod files;
mod fixture;
mod git;
mod herdr;
mod inline;
mod input;
pub mod live;
mod port;
mod terminal;
// Read on Windows only; tested everywhere.
#[cfg_attr(not(windows), allow(dead_code))]
mod vt_parse;
#[cfg(windows)]
mod windows_input;

pub use live::LiveSession;
pub use terminal::{
    PreviewExit, ScreenMode, TerminalSession, run_live, run_preview, run_preview_mode,
};
