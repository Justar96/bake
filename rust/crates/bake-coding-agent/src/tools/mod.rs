//! Building blocks for Pi's coding tools (v1.1.0; see the crate's `NOTICE`).
//!
//! Output handling is available independently of tool dispatch and process
//! execution. No tool is registered by this module.

pub mod output_accumulator;
pub mod truncate;

mod output_files;
mod utf8;
