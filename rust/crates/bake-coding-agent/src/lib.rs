//! Pi's coding-agent services for Bake's Rust agent.
//!
//! `bake-coding-agent` ports Pi's `packages/coding-agent` (release v1.1.0,
//! revision `abe508e1b89912adde45528136c3221eb69acdd7`, MIT; see the crate's
//! `NOTICE`). It holds Pi's session store, tool-output utilities, and the
//! headless agent runtime that `bake-rs -p` uses, plus the retained
//! CLIProxyAPI route. [`session`] reads and writes Pi's session format and
//! [`home`] places it under the Bake home (D25, D32).
//!
//! | Module | Source |
//! |---|---|
//! | [`session`] | Pi `src/core/session-manager.ts`, `session-cwd.ts`, `messages.ts`, `src/utils/paths.ts` |
//! | [`tools`] | Pi `src/core/tools/{truncate,output-accumulator}.ts`, `src/utils/output-files.ts` |
//! | [`home`] | Bake: `scripts/release/bake`, `packages/util/home-paths` |
//! | [`agent_session`] | Pi `src/core/agent-session.ts`, `src/core/sdk.ts` |
//! | [`system_prompt`] | Pi `src/core/system-prompt.ts`, `src/core/resource-loader.ts` (context files) |
//! | [`model_registry`] | Pi `src/core/{model-runtime,provider-composer,model-config,model-resolver}.ts`, `src/utils/json.ts` |
//! | [`auth_storage`] | Pi `src/core/auth-storage.ts` (read-only) |
//! | [`config_value`] | Pi `src/core/resolve-config-value.ts` |
//! | [`settings`] | Pi `src/core/settings-manager.ts` (default model and thinking level) |
//! | [`print_mode`], [`json_event`] | Pi `src/modes/print-mode.ts`, `src/modes/json-event.ts` |
//! | [`cli`] | Pi `src/main.ts`, `src/cli/args.ts`, `src/cli/initial-message.ts` |
//! | [`owned_work`] | Bake: the gate shutdown closes over owned work |
//! | [`cliproxyapi`] | Bake: `apps/tui/packages/app/src/cliproxyapi.ts`, the retained CLIProxyAPI route and its read-only D25 import |

pub mod agent_session;
pub mod auth_storage;
pub mod cli;
pub mod cliproxyapi;
pub mod config_value;
pub mod home;
pub mod json_event;
pub mod model_registry;
pub mod owned_work;
pub mod print_mode;
pub mod session;
pub mod settings;
pub mod system_prompt;
#[cfg(test)]
mod test_support;
pub mod tools;
