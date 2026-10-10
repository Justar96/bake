//! The agent loop and stateful agent for Bake's Rust runtime.
//!
//! `bake-agent` ports Pi's `agent` package (`packages/agent`, release
//! v1.1.0, revision `abe508e1b89912adde45528136c3221eb69acdd7`, MIT; see the
//! crate's `NOTICE`) onto `bake-ai`'s messages, providers, and event streams.
//!
//! | Module | Pi source |
//! |---|---|
//! | [`types`] | `packages/agent/src/types.ts` |
//! | [`hook`] | constructors for the hook types of `types.ts` |
//! | [`agent_loop`](mod@agent_loop) | `packages/agent/src/agent-loop.ts` |
//! | [`agent`] | `packages/agent/src/agent.ts` |
//! | [`stream_fn`] | `packages/agent/src/stream-fn.ts` |
//! | [`validation`] | `packages/ai/src/utils/validation.ts` |
//!
//! Pi's `proxy.ts` (a stream function over an HTTP proxy server) is not
//! ported. Everything runs on Tokio: [`Agent`] runs each prompt on a task it
//! owns, and the low-level loop polls tools on its own task.
#![warn(missing_docs)]

pub mod agent;
pub mod agent_loop;
pub mod hook;
pub mod stream_fn;
pub mod types;
pub mod validation;

pub use agent::{Agent, AgentError, AgentInitialState, AgentOptions, PromptInput, Subscription};
pub use agent_loop::{
    AgentLoopError, AgentLoopStream, RunToolCallOptions, agent_loop, agent_loop_continue,
    run_agent_loop, run_agent_loop_continue, run_tool_call,
};
pub use stream_fn::{default_stream_fn, registry_stream_fn, set_default_stream_fn};
pub use types::*;
