//! What the view and an agent runtime say to each other. The view sends a
//! [`Submission`] or a cancel as an [`crate::state::Effect`]; the runtime
//! answers with [`RuntimeUpdate`]s, which arrive as
//! [`crate::state::Msg::Runtime`] in the order it sent them.
//!
//! The runtime owns the transcript's content: a submitted prompt reaches the
//! transcript only when the runtime commits it, and a turn starts and ends
//! only when the runtime says so.

use crate::state::Outcome;
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
}
