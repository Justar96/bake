//! What the view and an agent runtime say to each other. The view sends a
//! [`Submission`] or a cancel as an [`crate::state::Effect`]; the runtime
//! answers with [`RuntimeUpdate`]s, which arrive as
//! [`crate::state::Msg::Runtime`] in the order it sent them.
//!
//! The runtime owns the transcript's content: a submitted prompt reaches the
//! transcript only when the runtime commits it, and a turn starts and ends
//! only when the runtime says so.

use crate::paste::Image;
use crate::state::Outcome;
use crate::status::ContextUsage;
use crate::transcript::Row;

/// A prompt the user sent, and how it reaches the agent: chosen from the
/// composer's mode, as the TypeScript `submitInput` chooses from the agent's
/// status.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Submission {
    /// Starts a turn while the agent is idle.
    FollowUp(String),
    /// Joins the running turn at its next step.
    Steer(String),
}

impl Submission {
    /// The prompt's text, its placeholders expanded.
    pub fn text(&self) -> &str {
        match self {
            Self::FollowUp(text) | Self::Steer(text) => text,
        }
    }
}

/// A submission with the images staged with it, in placeholder order.
/// Only a live session's idle composer sends images. A live runtime
/// answers each by its `id` with [`RuntimeUpdate::Admitted`] or
/// [`RuntimeUpdate::Refused`]; until then the view keeps the draft it came
/// from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Prompt {
    pub id: u64,
    pub submission: Submission,
    pub images: Vec<Image>,
}

impl From<Submission> for Prompt {
    fn from(submission: Submission) -> Self {
        Self {
            id: 0,
            submission,
            images: Vec::new(),
        }
    }
}

/// What the status line reports about the session: its model as
/// `provider/id`, its thinking level, and how much of the model's context
/// the last response used. `None` leaves a field out.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SessionStatus {
    pub model: Option<String>,
    pub thinking: Option<String>,
    pub context: Option<ContextUsage>,
}

/// When a waiting prompt is admitted, as the TypeScript inbox targets it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    /// At the running turn's next step.
    NextStep,
    /// When the next turn starts.
    NextTurn,
}

/// A prompt the runtime holds but has not admitted yet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pending {
    pub text: String,
    pub target: Target,
}

/// One change the runtime reports.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RuntimeUpdate {
    /// A turn started. The seed fixes the bar's word for the whole turn.
    TurnStarted { seed: String },
    /// What the running turn is doing now, such as `thinking` or
    /// `running bash`, for the bar.
    Phase(String),
    /// The rows still being written, which replace the previous live rows.
    /// They are drawn after every committed row.
    Live(Vec<Row>),
    /// Rows settled for good. They replace the live rows, so a streamed row
    /// appears once, whether still live or committed.
    Commit(Vec<Row>),
    /// The running turn ended; anything still live was committed first.
    TurnEnded(Outcome),
    /// Every prompt still waiting, in the order it will be admitted. Sent
    /// whenever the list changes; an empty list clears the panel.
    Pending(Vec<Pending>),
    /// The prompt with this id reached the agent, or its queue, so the view
    /// forgets the draft it came from.
    Admitted(u64),
    /// The prompt with this id will not run, and why. The view puts back
    /// the draft it came from, placeholders and staged images included,
    /// ahead of anything typed since.
    Refused { id: u64, reason: String },
    /// Queued prompts a cancel dropped, as text, oldest first. They go back
    /// into the draft ahead of what it holds, as Pi's
    /// `restoreQueuedMessagesToEditor` does, so no submission is lost.
    Returned(Vec<String>),
    /// The session's model, thinking level, and context use changed.
    Status(SessionStatus),
}
