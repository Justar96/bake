//! Pi's coding tools and their building blocks (v1.1.0; see the crate's
//! `NOTICE`).
//!
//! | Module | Pi source |
//! |---|---|
//! | [`read`], [`write`](mod@write), [`edit`], [`ls`], [`find`], [`grep`] | `src/core/tools/{read,write,edit,ls,find,grep}.ts` |
//! | [`edit_diff`] | `src/core/tools/edit-diff.ts`, with jsdiff 8.0.4's line diff and patch |
//! | [`path_utils`] | `src/core/tools/path-utils.ts`, `src/utils/paths.ts` |
//! | [`definition`] | `src/core/tools/tool-definition-wrapper.ts` |
//! | [`index`] | `src/core/tools/index.ts`, and the tool selection of `settings-manager.ts`, `sdk.ts`, and `mcp-servers.ts` |
//! | [`image`] | `src/utils/{mime,exif-orientation,image-convert,image-resize-core,image-resize,image-process,tool-result-images}.ts` |
//! | [`bash`] | `src/core/tools/bash.ts`, `src/utils/{shell,child-process}.ts`; owns each command's process tree |
//! | [`truncate`], [`output_accumulator`], [`file_mutation_queue`] | `src/core/tools/{truncate,output-accumulator,file-mutation-queue}.ts` |
//!
//! Every tool is a [`bake_agent::AgentTool`] whose names, descriptions,
//! schemas, results, and errors are Pi's. Blocking file work runs on
//! Tokio's blocking pool and is awaited, aborted or not; `write` and `edit`
//! run inside the file's mutation queue. `grep` and `find` spawn `rg` and
//! `fd` from `PATH` ([`process`]) and kill and reap them on abort. Pi's `bash`
//! tool joins through [`index::ToolsOptions::bash`].

pub mod bash;
pub mod definition;
pub mod edit;
pub mod edit_diff;
pub mod file_mutation_queue;
pub mod find;
pub mod grep;
pub mod image;
pub mod index;
pub mod ls;
pub mod output_accumulator;
pub mod path_utils;
pub mod process;
pub mod read;
pub mod truncate;
pub mod write;

// The recorded cases use POSIX paths, so the replay runs on Unix only.
#[cfg(all(test, unix))]
mod golden_replay;
mod head;
pub(crate) mod js;
mod jsdiff;
mod node_error;
pub(crate) mod output_files;
mod utf8;
