//! External hook processes in the Claude Code and Codex `hooks.json`
//! formats.
//!
//! Ported from Bake 0.3's hooks group (`packages/hooks/`): the shared
//! engine `hook-protocol` ([`matcher`], [`codec`], [`merge`], [`runner`])
//! and both bridges, `hooks-claude-code` and `hooks-codex` ([`config`] and
//! the decision mapping in [`engine`]), with their tests. The hook points
//! are Pi v1.3.0's extension events (`src/core/extensions/types.ts`,
//! `runner.ts`, and their callers in `agent-session.ts`): `session_start`,
//! `input`, `tool_call`, `tool_result`, and `agent_before_settle`; the
//! session runs them in `agent_session/hooks.rs`.
//!
//! | Event | When | What its hooks can do |
//! |---|---|---|
//! | `SessionStart` | before the first prompt the process sends in the session | add context |
//! | `UserPromptSubmit` | before a prompt, steer, or follow-up is sent or queued | block it, or add context |
//! | `PreToolUse` | before a tool call, after the plugins' vetoes | deny it; Claude Code hooks may also ask the user |
//! | `PostToolUse` | after a tool call, before its result is recorded | replace the result with feedback, or add context |
//! | `Stop` | when a run would end | continue it with a reason |
//!
//! A `{"continue": false}` stops as each bridge stopped in 0.3: a prompt
//! is blocked; a Claude Code tool hook ends the run once the calls of its
//! batch finish (a `PreToolUse` one also denies the call); a Codex
//! `PostToolUse` hook replaces the result; and on `Stop` it drops a block's
//! continuation. Codex `PreToolUse` hooks honor only blocks.
//!
//! Each event's hooks run one after another in file order, Claude Code's
//! file first; a dialect whose hooks block, deny, or ask ends the event
//! before the next dialect's hooks run. Every evaluation that ran a hook
//! is written to the session file as a Pi `custom` entry,
//! [`DECISION_ENTRY_TYPE`], before its decision takes effect, and added
//! context reaches the model as a `custom_message`, [`CONTEXT_MESSAGE_TYPE`],
//! so the session file reconstructs what the model saw.
//!
//! Configuration is `settings.json`'s `hooks` member ([`home`]); nothing
//! runs unless it names a file.
//!
//! Not ported: 0.3's `SubagentStart` and `SubagentStop` events, Claude
//! Code's `${CLAUDE_PLUGIN_ROOT}` substitution, `systemMessage` display,
//! and `updatedInput`, which is recorded and ignored as in 0.3.

pub mod codec;
pub mod config;
pub(crate) mod engine;
pub mod home;
pub mod matcher;
pub mod merge;
pub mod runner;

use std::sync::Arc;
use std::time::Duration;

use bake_sandbox::SandboxProvider;

pub use config::{CommandHook, HookGroups, MatcherGroup, ParsedHooks, parse_hooks};
pub use engine::{POST_TOOL_USE_BLOCKED, PRE_TOOL_USE_BLOCKED, PROMPT_BLOCKED, STOP_BLOCKED};
pub use home::{HookFile, load_hook_files};
pub use runner::{DEFAULT_HOOK_TIMEOUT, MAX_HOOK_OUTPUT_BYTES};

use crate::permissions::{Approver, StandingPermissions};

/// The `customType` of the entry recording each hook evaluation.
pub const DECISION_ENTRY_TYPE: &str = "bake.hook.decision";

/// The `customType` of the message carrying a hook's context or a `Stop`
/// hook's reason to the model, and of the note a run ends with when its
/// `Stop` hooks reach their cap (`details.capped`).
pub const CONTEXT_MESSAGE_TYPE: &str = "bake.hook.context";

/// A `hooks.json` format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookDialect {
    /// Claude Code's.
    ClaudeCode,
    /// Codex's.
    Codex,
}

impl HookDialect {
    /// The wire name: `claude-code` or `codex`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ClaudeCode => "claude-code",
            Self::Codex => "codex",
        }
    }

    /// The product's name.
    pub fn title(self) -> &'static str {
        match self {
            Self::ClaudeCode => "Claude Code",
            Self::Codex => "Codex",
        }
    }
}

/// A hook point.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookEvent {
    SessionStart,
    UserPromptSubmit,
    PreToolUse,
    PostToolUse,
    Stop,
}

impl HookEvent {
    /// Every event, in the order a config is read.
    pub const ALL: [Self; 5] = [
        Self::SessionStart,
        Self::UserPromptSubmit,
        Self::PreToolUse,
        Self::PostToolUse,
        Self::Stop,
    ];

    /// The name `hooks.json` keys it by.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SessionStart => "SessionStart",
            Self::UserPromptSubmit => "UserPromptSubmit",
            Self::PreToolUse => "PreToolUse",
            Self::PostToolUse => "PostToolUse",
            Self::Stop => "Stop",
        }
    }

    /// Whether its matchers select anything: the tool name, or the
    /// session's source.
    pub fn has_matcher(self) -> bool {
        !matches!(self, Self::UserPromptSubmit | Self::Stop)
    }
}

/// Which events a session runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookScope {
    /// Every event: a top-level session.
    Session,
    /// `PreToolUse` and `PostToolUse` only: a subagent, whose prompts come
    /// from its parent's tool calls.
    ToolsOnly,
}

/// What [`crate::agent_session::AgentSession::set_hooks`] runs.
#[derive(Clone)]
pub struct HooksOptions {
    /// The hooks files, in the order their dialects run.
    pub files: Vec<HookFile>,
    /// Which events run.
    pub scope: HookScope,
    /// The session's standing permission: its sandbox mode when a hook
    /// starts confines that hook (D24).
    pub standing: Arc<StandingPermissions>,
    /// The provider that confines it; the one the session's `bash` uses.
    pub sandbox: Arc<dyn SandboxProvider>,
    /// Who answers a Claude Code `PreToolUse` hook's `ask`; the session's
    /// approval policy applies first, and none available denies the call.
    pub approver: Arc<dyn Approver>,
    /// The timeout of a hook that sets none ([`DEFAULT_HOOK_TIMEOUT`]).
    pub default_timeout: Duration,
}

impl std::fmt::Debug for HooksOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HooksOptions")
            .field("files", &self.files)
            .field("scope", &self.scope)
            .field("sandbox", &self.sandbox.name())
            .field("default_timeout", &self.default_timeout)
            .finish_non_exhaustive()
    }
}
