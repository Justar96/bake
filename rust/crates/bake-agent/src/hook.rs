//! Constructors for the hook types.
//!
//! A closure stored as one of the boxed hook types must be generic over the
//! lifetime of the views it receives. Rust infers that only when the closure
//! is passed straight to a function bounded by the hook's signature, so each
//! hook type has a constructor here. For example:
//!
//! ```
//! use bake_agent::{AgentTurnDecision, hook};
//!
//! let finish = hook::finish_turn(|turn, _signal| {
//!     Box::pin(async move {
//!         (!turn.tool_results.is_empty()).then_some(AgentTurnDecision::End)
//!     })
//! });
//! # drop(finish);
//! ```

use std::sync::Arc;

use bake_ai::{AbortSignal, Message};

use crate::types::{
    AfterToolCall, AfterToolCallContext, AfterToolCallResult, AgentLoopTurnUpdate, AgentMessage,
    AgentRequestUpdate, AgentTurnContext, AgentTurnDecision, BeforeToolCall, BeforeToolCallContext,
    BeforeToolCallResult, BoxFuture, ConvertToLlm, FinishTurn, GetApiKey, GetMessages,
    PrepareNextTurn, PrepareRequest, PrepareRequestContext, TransformContext,
};

/// A [`ConvertToLlm`].
pub fn convert_to_llm<F>(f: F) -> ConvertToLlm
where
    F: for<'a> Fn(&'a [AgentMessage]) -> BoxFuture<'a, Vec<Message>> + Send + Sync + 'static,
{
    Arc::new(f)
}

/// A [`TransformContext`].
pub fn transform_context<F>(f: F) -> TransformContext
where
    F: Fn(Vec<AgentMessage>, Option<AbortSignal>) -> BoxFuture<'static, Vec<AgentMessage>>
        + Send
        + Sync
        + 'static,
{
    Arc::new(f)
}

/// A [`GetApiKey`].
pub fn get_api_key<F>(f: F) -> GetApiKey
where
    F: for<'a> Fn(&'a str) -> BoxFuture<'a, Option<String>> + Send + Sync + 'static,
{
    Arc::new(f)
}

/// A [`GetMessages`].
pub fn get_messages<F>(f: F) -> GetMessages
where
    F: Fn() -> BoxFuture<'static, Vec<AgentMessage>> + Send + Sync + 'static,
{
    Arc::new(f)
}

/// A [`BeforeToolCall`].
pub fn before_tool_call<F>(f: F) -> BeforeToolCall
where
    F: for<'a> Fn(
            BeforeToolCallContext<'a>,
            Option<AbortSignal>,
        ) -> BoxFuture<'a, Result<Option<BeforeToolCallResult>, String>>
        + Send
        + Sync
        + 'static,
{
    Arc::new(f)
}

/// An [`AfterToolCall`].
pub fn after_tool_call<F>(f: F) -> AfterToolCall
where
    F: for<'a> Fn(
            AfterToolCallContext<'a>,
            Option<AbortSignal>,
        ) -> BoxFuture<'a, Result<Option<AfterToolCallResult>, String>>
        + Send
        + Sync
        + 'static,
{
    Arc::new(f)
}

/// A [`FinishTurn`].
pub fn finish_turn<F>(f: F) -> FinishTurn
where
    F: for<'a> Fn(
            AgentTurnContext<'a>,
            Option<AbortSignal>,
        ) -> BoxFuture<'a, Option<AgentTurnDecision>>
        + Send
        + Sync
        + 'static,
{
    Arc::new(f)
}

/// A [`PrepareRequest`].
pub fn prepare_request<F>(f: F) -> PrepareRequest
where
    F: for<'a> Fn(
            PrepareRequestContext<'a>,
            Option<AbortSignal>,
        ) -> BoxFuture<'a, Option<AgentRequestUpdate>>
        + Send
        + Sync
        + 'static,
{
    Arc::new(f)
}

/// A [`PrepareNextTurn`].
pub fn prepare_next_turn<F>(f: F) -> PrepareNextTurn
where
    F: for<'a> Fn(AgentTurnContext<'a>) -> BoxFuture<'a, Option<AgentLoopTurnUpdate>>
        + Send
        + Sync
        + 'static,
{
    Arc::new(f)
}
