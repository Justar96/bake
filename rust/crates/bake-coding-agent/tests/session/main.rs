//! Pi's session-manager tests, ported, and the Pi golden files.
//!
//! Each module names the Pi test file it ports (Pi `packages/coding-agent/test/`,
//! v1.1.0) and each test the Pi test it follows. Tests that drive Pi's
//! `AgentSession`, CLI, or compaction preparation, rather than the manager,
//! are not ported here.

mod build_context;
mod context_edit;
mod custom_session_id;
mod file_operations;
mod golden;
mod held_values;
mod js_semantics;
mod labels;
mod load_entries;
mod migration;
mod regressions;
mod robustness;
mod save_entry;
mod scaling;
mod session_cwd;
mod session_id;
mod session_info;
mod support;
mod tree_traversal;
