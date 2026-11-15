//! Unanswered tool calls: the provider-visible part of the unfinished-work
//! projection.
//!
//! Pi's `transformMessages` (`packages/ai/src/api/transform-messages.ts`,
//! v1.3.0, revision `71023aec1546d6d677a65669c5665c0927ce0dd0`; the rule is
//! unchanged since v1.1.0) closes each tool call it finds no result for with
//! a synthetic `No result provided` error result, in every request it builds,
//! and leaves the session file unchanged. This module derives, from the
//! session entries alone, the calls that rule closes: it answers "which tool
//! calls does the next request built from this leaf answer with a synthetic
//! result", with the entry that holds each call.
//!
//! # Provider-visible, not durable history
//!
//! The answer covers the *context* of the leaf, the messages
//! [`build_session_projection`] rebuilds, read as the agent reads them
//! ([`convert_to_llm`]). It is not the *path history*, every entry from the
//! root to the leaf. The two differ, and a consumer must pick the one its
//! question needs:
//!
//! | A tool call on the path to the leaf that | Path history | This projection |
//! |---|---|---|
//! | is before the latest compaction's kept boundary | present | absent |
//! | sits in an entry a context edit omits or whose content it replaces | present | absent |
//! | sits in an assistant message that stopped with `error` or `aborted` | present | absent: the request drops the message |
//! | sits in an assistant message that does not read as one (a missing `usage`, a malformed block) | present | absent: the request drops the message |
//! | has its result only after a later user, custom, summary, or included `bashExecution` message, or after a later assistant message | answered in the file | present: the request no longer pairs that result with the call |
//! | has no result, with only system messages or excluded `bashExecution` messages after it | unanswered | present |
//!
//! So an entry here means the model will see the call closed as failed. It
//! does not mean the call's effects did not happen: a call can have run, and
//! even have its result in the file, and still be listed; a call can be
//! absent and still have run without a recorded result. Deciding whether a
//! tool ran, or whether repeating it is safe, needs the path history and the
//! tool's replay class, which this projection does not provide.
//!
//! # Ids
//!
//! Calls and results are matched by their stored ids, as Pi matches them
//! when it does not rewrite ids (a request for the model that wrote the
//! call, or a provider whose rewrite is one-to-one). A provider that maps two
//! distinct stored ids to one id may pair results differently.

use std::collections::HashSet;

use bake_ai::types::{Message, StopReason};

use crate::session::context::{LeafSelector, SessionProjection, build_session_projection};
use crate::session::entry::SessionEntry;
use crate::session::messages::{AgentMessage, convert_to_llm};

/// A tool call the next request closes with Pi's synthetic error result.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct UnansweredToolCall {
    /// The id of the `message` entry holding the assistant message.
    pub entry_id: String,
    /// The call's stored `toolCall` id.
    pub tool_call_id: String,
    /// The called tool's name.
    pub tool_name: String,
}

/// The tool calls in `projection` the next request answers with Pi's
/// synthetic `No result provided` result, in request order: one per call
/// block, so a block repeated within one message, which Pi answers once per
/// block, is listed once per block. See the [module](self) for what the
/// answer does not cover.
pub fn unanswered_tool_calls(projection: &SessionProjection) -> Vec<UnansweredToolCall> {
    let mut scan = Scan::default();
    for entry in &projection.entries {
        let live: Vec<_> = entry.messages.iter().map(AgentMessage::to_agent).collect();
        for message in convert_to_llm(&live) {
            scan.message(entry.source_entry.id(), message);
        }
    }
    scan.close();
    scan.unanswered
}

/// [`unanswered_tool_calls`] of the projection for `leaf`.
pub fn build_unanswered_tool_calls(
    entries: &[SessionEntry],
    leaf: LeafSelector<'_>,
) -> Vec<UnansweredToolCall> {
    unanswered_tool_calls(&build_session_projection(entries, leaf))
}

/// The second pass of Pi's `transformMessages`, recording the calls it
/// would answer instead of building a request.
#[derive(Default)]
struct Scan {
    pending: Vec<UnansweredToolCall>,
    answered: HashSet<String>,
    unanswered: Vec<UnansweredToolCall>,
}

impl Scan {
    fn message(&mut self, entry_id: &str, message: Message) {
        match message {
            Message::Assistant(assistant) => {
                self.close();
                if matches!(
                    assistant.stop_reason,
                    StopReason::Error | StopReason::Aborted
                ) {
                    return;
                }
                let calls: Vec<UnansweredToolCall> = assistant
                    .tool_calls()
                    .map(|call| UnansweredToolCall {
                        entry_id: entry_id.to_owned(),
                        tool_call_id: call.id.clone(),
                        tool_name: call.name.clone(),
                    })
                    .collect();
                if !calls.is_empty() {
                    self.pending = calls;
                    self.answered.clear();
                }
            }
            Message::ToolResult(result) => {
                self.answered.insert(result.tool_call_id);
            }
            // Pi holds a system message back until the pending calls close;
            // it never closes them.
            Message::System(_) => {}
            Message::User(_) => self.close(),
        }
    }

    fn close(&mut self) {
        if self.pending.is_empty() {
            return;
        }
        for call in self.pending.drain(..) {
            if !self.answered.contains(&call.tool_call_id) {
                self.unanswered.push(call);
            }
        }
        self.answered.clear();
    }
}
