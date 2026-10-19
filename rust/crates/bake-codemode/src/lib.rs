//! Pi's codemode for Bake's Rust agent: the pieces of the script environment
//! that need no engine. [`Source`] parses a script's `// @options:` line,
//! [`render_declarations`] renders TypeScript declarations from JSON Schema,
//! [`to_codemode_identifier`] maps tool names to script identifiers, and
//! [`Bm25Ranker`] ranks tools for `searchTools()`. Like Pi's
//! `packages/codemode`, the crate has no agent types.

#![warn(missing_docs)]
#![forbid(unsafe_code)]

mod declarations;
mod identifier;
mod js;
mod source;
mod tool_search;

pub use declarations::{
    DEFAULT_INPUT_SCHEMA_MAX_CHARS, ScriptFunction, render_declarations, render_tool_output_type,
    render_tool_sample, render_tool_signature, schema_to_type,
};
pub use identifier::to_codemode_identifier;
pub use source::{CODEMODE_OPTIONS_PREFIX, MAX_TIMEOUT_MS, Source, SourceError, SourceOptions};
pub use tool_search::{
    Bm25Ranker, DEFAULT_TOOL_SEARCH_LIMIT, ToolNamespace, ToolSearchDocument, ToolSearchMatch,
    create_tool_search_document, tokenize,
};
