//! Questions the model puts to the person: the `ask_user` tool and the
//! queue a frontend answers it from.
//!
//! [`QuestionBroker`] keeps the discipline of the approval broker
//! ([`crate::permissions::ApprovalBroker`]) for a different answer: one
//! frontend connects and receives the whole queue, oldest first, whenever
//! it changes; the oldest is the one shown; an answer settles only the
//! request its generation names; a request whose call is cancelled is
//! withdrawn; and closing the broker settles every request as declined.
//! Before a frontend connects, or after it has gone, nobody can answer, so
//! the tool says so at once instead of waiting: print mode has no person
//! to ask.
//!
//! A request also carries who asks: `None` for this session's own model,
//! or the callsign of a spawned agent whose question this session
//! forwards ([`crate::spawned`]).

mod tool;

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use bake_ai::AbortSignal;
use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;

pub use tool::{TOOL_NAME, tool};

/// The most questions one call may ask.
pub const MAX_QUESTIONS: usize = 4;
/// The most options one question may offer.
pub const MAX_OPTIONS: usize = 6;

/// One offered answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct QuestionOption {
    pub label: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
}

/// One question. With no options it takes a typed answer; with options
/// the person picks one, or several with `multi_select`, or types their
/// own.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Question {
    pub question: String,
    /// A short label for it, up to 16 characters.
    #[serde(default)]
    pub header: String,
    #[serde(default)]
    pub options: Vec<QuestionOption>,
    #[serde(default)]
    pub multi_select: bool,
}

/// A queued request as a frontend shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuestionPrompt {
    /// Unique for the broker's life.
    pub generation: u64,
    /// The tool call asking.
    pub call_id: String,
    /// The spawned agent asking through this session, if not its own model.
    pub from: Option<String>,
    pub questions: Vec<Question>,
}

/// The person's answer to one request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QuestionAnswer {
    /// One entry per question: the labels chosen, or the text typed.
    Answered(Vec<Vec<String>>),
    /// Dismissed without an answer.
    Declined,
}

/// Receives the queue, oldest first, whenever it changes; returns `false`
/// once the frontend is gone.
pub type QuestionListener = Box<dyn Fn(Vec<QuestionPrompt>) -> bool + Send + Sync>;

enum Phase {
    Unconnected,
    Open(Arc<QuestionListener>),
    Closed,
}

struct Waiting {
    prompt: QuestionPrompt,
    answer: oneshot::Sender<QuestionAnswer>,
}

struct Inner {
    phase: Phase,
    queue: VecDeque<Waiting>,
    last: u64,
    announcing: bool,
    changed: bool,
}

/// Queues questions for a person; see the [module documentation](self).
#[derive(Clone)]
pub struct QuestionBroker {
    inner: Arc<Mutex<Inner>>,
}

impl Default for QuestionBroker {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for QuestionBroker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("QuestionBroker")
            .field("pending", &self.pending().len())
            .finish_non_exhaustive()
    }
}

fn lock(inner: &Mutex<Inner>) -> MutexGuard<'_, Inner> {
    inner.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Why nobody can answer.
pub const NO_PERSON: &str = "no person can answer questions in this session (it runs without a terminal or client to ask); decide yourself and say what you assumed";

impl QuestionBroker {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner {
                phase: Phase::Unconnected,
                queue: VecDeque::new(),
                last: 0,
                announcing: false,
                changed: false,
            })),
        }
    }

    /// Connects the one frontend. Returns `false` when one already has, or
    /// the broker is closed.
    pub fn connect(&self, listener: QuestionListener) -> bool {
        {
            let mut inner = lock(&self.inner);
            if !matches!(inner.phase, Phase::Unconnected) {
                return false;
            }
            inner.phase = Phase::Open(Arc::new(listener));
        }
        announce(&self.inner);
        true
    }

    /// Whether a frontend can answer now.
    pub fn is_open(&self) -> bool {
        matches!(lock(&self.inner).phase, Phase::Open(_))
    }

    /// Asks `questions` and waits for the answer, or for `cancel`.
    /// Fails at once when nobody can answer.
    pub async fn ask(
        &self,
        call_id: &str,
        from: Option<String>,
        questions: Vec<Question>,
        cancel: Option<AbortSignal>,
    ) -> Result<QuestionAnswer, String> {
        let (answer, answered) = oneshot::channel();
        let generation = {
            let mut inner = lock(&self.inner);
            if !matches!(inner.phase, Phase::Open(_)) {
                return Err(NO_PERSON.to_owned());
            }
            inner.last += 1;
            let generation = inner.last;
            inner.queue.push_back(Waiting {
                prompt: QuestionPrompt {
                    generation,
                    call_id: call_id.to_owned(),
                    from,
                    questions,
                },
                answer,
            });
            generation
        };
        announce(&self.inner);
        let cancelled = async {
            match &cancel {
                Some(signal) => signal.cancelled().await,
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            biased;
            () = cancelled => {
                self.withdraw(generation);
                Err("the question was withdrawn".to_owned())
            }
            answer = answered => Ok(answer.unwrap_or(QuestionAnswer::Declined)),
        }
    }

    fn withdraw(&self, generation: u64) {
        let removed = {
            let mut inner = lock(&self.inner);
            let before = inner.queue.len();
            inner.queue.retain(|w| w.prompt.generation != generation);
            inner.queue.len() != before
        };
        if removed {
            announce(&self.inner);
        }
    }

    /// Settles the shown request, the oldest, if `generation` names it.
    /// An answer with the wrong number of entries settles nothing.
    pub fn answer(&self, generation: u64, answer: QuestionAnswer) -> bool {
        let settled = {
            let mut inner = lock(&self.inner);
            let fits = inner.queue.front().is_some_and(|front| {
                front.prompt.generation == generation
                    && match &answer {
                        QuestionAnswer::Answered(entries) => {
                            entries.len() == front.prompt.questions.len()
                        }
                        QuestionAnswer::Declined => true,
                    }
            });
            if !fits {
                return false;
            }
            inner
                .queue
                .pop_front()
                .is_some_and(|waiting| waiting.answer.send(answer).is_ok())
        };
        announce(&self.inner);
        settled
    }

    /// The queue, oldest first.
    pub fn pending(&self) -> Vec<QuestionPrompt> {
        lock(&self.inner)
            .queue
            .iter()
            .map(|w| w.prompt.clone())
            .collect()
    }

    /// Disconnects the frontend: every queued request is declined and
    /// later ones fail at once.
    pub fn close(&self) {
        let mut inner = lock(&self.inner);
        inner.phase = Phase::Closed;
        for waiting in inner.queue.drain(..) {
            let _ = waiting.answer.send(QuestionAnswer::Declined);
        }
    }
}

/// Tells the listener what waits now, or leaves that to the caller
/// already announcing, so it sees the queue's states in order. No lock is
/// held while it runs; a listener that is gone or panics closes the broker.
fn announce(shared: &Mutex<Inner>) {
    {
        let mut inner = lock(shared);
        if inner.announcing {
            inner.changed = true;
            return;
        }
        inner.announcing = true;
    }
    loop {
        let (listener, snapshot) = {
            let mut inner = lock(shared);
            inner.changed = false;
            let Phase::Open(listener) = &inner.phase else {
                inner.announcing = false;
                return;
            };
            let snapshot: Vec<QuestionPrompt> =
                inner.queue.iter().map(|w| w.prompt.clone()).collect();
            (Arc::clone(listener), snapshot)
        };
        let alive = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| listener(snapshot)))
            .unwrap_or(false);
        let mut inner = lock(shared);
        if !alive {
            inner.phase = Phase::Closed;
            for waiting in inner.queue.drain(..) {
                let _ = waiting.answer.send(QuestionAnswer::Declined);
            }
            inner.announcing = false;
            return;
        }
        if !inner.changed {
            inner.announcing = false;
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn question(text: &str) -> Question {
        Question {
            question: text.into(),
            header: String::new(),
            options: vec![
                QuestionOption {
                    label: "Yes".into(),
                    description: String::new(),
                },
                QuestionOption {
                    label: "No".into(),
                    description: String::new(),
                },
            ],
            multi_select: false,
        }
    }

    #[tokio::test]
    async fn nobody_to_ask_fails_at_once() {
        let broker = QuestionBroker::new();
        assert_eq!(
            broker.ask("c1", None, vec![question("q")], None).await,
            Err(NO_PERSON.to_owned())
        );
    }

    #[tokio::test]
    async fn the_oldest_is_answered_by_its_generation() {
        let broker = QuestionBroker::new();
        let seen = Arc::new(Mutex::new(Vec::new()));
        {
            let seen = Arc::clone(&seen);
            assert!(broker.connect(Box::new(move |queue| {
                seen.lock().unwrap().push(queue.len());
                true
            })));
        }
        let first = tokio::spawn({
            let broker = broker.clone();
            async move { broker.ask("c1", None, vec![question("one")], None).await }
        });
        let second = tokio::spawn({
            let broker = broker.clone();
            async move {
                broker
                    .ask("c2", Some("alpha-1".into()), vec![question("two")], None)
                    .await
            }
        });
        while broker.pending().len() < 2 {
            tokio::task::yield_now().await;
        }
        let [one, two] = <[QuestionPrompt; 2]>::try_from(broker.pending()).unwrap();
        assert!(
            !broker.answer(two.generation, QuestionAnswer::Declined),
            "not shown yet"
        );
        assert!(
            !broker.answer(one.generation, QuestionAnswer::Answered(vec![])),
            "wrong count"
        );
        assert!(broker.answer(
            one.generation,
            QuestionAnswer::Answered(vec![vec!["Yes".into()]])
        ));
        assert_eq!(
            first.await.unwrap(),
            Ok(QuestionAnswer::Answered(vec![vec!["Yes".into()]]))
        );
        assert_eq!(two.from.as_deref(), Some("alpha-1"));
        broker.close();
        assert_eq!(second.await.unwrap(), Ok(QuestionAnswer::Declined));
        assert_eq!(seen.lock().unwrap().last(), Some(&1));
    }

    #[tokio::test]
    async fn a_cancelled_call_withdraws_its_question() {
        let broker = QuestionBroker::new();
        assert!(broker.connect(Box::new(|_| true)));
        let controller = bake_ai::AbortController::new();
        let signal = controller.signal();
        let asking = tokio::spawn({
            let broker = broker.clone();
            async move {
                broker
                    .ask("c1", None, vec![question("q")], Some(signal))
                    .await
            }
        });
        while broker.pending().is_empty() {
            tokio::task::yield_now().await;
        }
        controller.abort();
        assert!(asking.await.unwrap().is_err());
        assert!(broker.pending().is_empty());
    }
}
