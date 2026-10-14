//! An asynchronous stream of events with a final result.
//!
//! Ported from Pi `packages/ai/src/utils/event-stream.ts` (v1.1.0). Pi's one
//! `EventStream` object both receives pushes and is iterated; here the
//! producer half is an [`EventSender`] and the consumer half an
//! [`EventStream`]. Dropping the sender ends the stream, so a producer task
//! that fails or is cancelled cannot leave a consumer waiting. Consumers read
//! with [`EventStream::next`] and await the final result with
//! [`EventStream::result`], which is `None` when the stream ended without one.

use std::collections::VecDeque;
use std::future::poll_fn;
use std::sync::{Arc, Mutex, MutexGuard};
use std::task::{Poll, Waker};
use std::time::Instant;

use crate::types::{AssistantMessage, AssistantMessageEvent};

struct Inner<T, R> {
    queue: VecDeque<T>,
    done: bool,
    result: Option<R>,
    event_wakers: Vec<Waker>,
    result_wakers: Vec<Waker>,
}

struct Shared<T, R> {
    inner: Mutex<Inner<T, R>>,
    is_complete: fn(&T) -> bool,
    extract_result: fn(&T) -> Option<R>,
}

impl<T, R> Shared<T, R> {
    fn lock(&self) -> MutexGuard<'_, Inner<T, R>> {
        // A panic while holding the lock cannot leave the queue half-updated,
        // so a poisoned lock is still consistent.
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

fn register(wakers: &mut Vec<Waker>, waker: &Waker) {
    if !wakers.iter().any(|known| known.will_wake(waker)) {
        wakers.push(waker.clone());
    }
}

/// Creates a connected sender and stream. `is_complete` marks the event that
/// ends the stream, and `extract_result` takes the final result from it.
pub fn event_channel<T, R>(
    is_complete: fn(&T) -> bool,
    extract_result: fn(&T) -> Option<R>,
) -> (EventSender<T, R>, EventStream<T, R>) {
    let shared = Arc::new(Shared {
        inner: Mutex::new(Inner {
            queue: VecDeque::new(),
            done: false,
            result: None,
            event_wakers: Vec::new(),
            result_wakers: Vec::new(),
        }),
        is_complete,
        extract_result,
    });
    (
        EventSender {
            shared: Arc::clone(&shared),
        },
        EventStream { shared },
    )
}

/// The producer half. Dropping it ends the stream without a result if none
/// was set.
pub struct EventSender<T, R> {
    shared: Arc<Shared<T, R>>,
}

impl<T, R> EventSender<T, R> {
    /// Queues `event`. After the completing event or [`EventSender::end`],
    /// pushes are ignored.
    pub fn push(&self, event: T) {
        let wakers = {
            let mut inner = self.shared.lock();
            if inner.done {
                return;
            }
            if (self.shared.is_complete)(&event) {
                inner.done = true;
                if inner.result.is_none() {
                    inner.result = (self.shared.extract_result)(&event);
                }
            }
            inner.queue.push_back(event);
            take_wakers(&mut inner)
        };
        wakers.into_iter().for_each(Waker::wake);
    }

    /// Ends the stream; `result` becomes the final result unless one is set.
    pub fn end(&self, result: Option<R>) {
        let wakers = {
            let mut inner = self.shared.lock();
            inner.done = true;
            if inner.result.is_none() {
                inner.result = result;
            }
            take_wakers(&mut inner)
        };
        wakers.into_iter().for_each(Waker::wake);
    }

    /// Whether the stream is done.
    pub fn is_done(&self) -> bool {
        self.shared.lock().done
    }

    /// Whether every consumer is gone, so further events reach no one.
    pub fn is_closed(&self) -> bool {
        Arc::strong_count(&self.shared) == 1
    }
}

impl<T, R> Drop for EventSender<T, R> {
    fn drop(&mut self) {
        self.end(None);
    }
}

fn take_wakers<T, R>(inner: &mut Inner<T, R>) -> Vec<Waker> {
    let mut wakers = std::mem::take(&mut inner.event_wakers);
    wakers.append(&mut inner.result_wakers);
    wakers
}

/// The consumer half. Clones share one queue: each event goes to exactly one
/// reader, in order.
pub struct EventStream<T, R> {
    shared: Arc<Shared<T, R>>,
}

impl<T, R> Clone for EventStream<T, R> {
    fn clone(&self) -> Self {
        Self {
            shared: Arc::clone(&self.shared),
        }
    }
}

impl<T, R: Clone> EventStream<T, R> {
    /// The next event, or `None` once the stream is done and drained.
    /// Cancelling this future loses no event.
    pub async fn next(&self) -> Option<T> {
        poll_fn(|cx| {
            let mut inner = self.shared.lock();
            if let Some(event) = inner.queue.pop_front() {
                return Poll::Ready(Some(event));
            }
            if inner.done {
                return Poll::Ready(None);
            }
            register(&mut inner.event_wakers, cx.waker());
            Poll::Pending
        })
        .await
    }

    /// The final result once the stream is done; `None` when it ended
    /// without one.
    pub async fn result(&self) -> Option<R> {
        poll_fn(|cx| {
            let mut inner = self.shared.lock();
            if inner.done {
                return Poll::Ready(inner.result.clone());
            }
            register(&mut inner.result_wakers, cx.waker());
            Poll::Pending
        })
        .await
    }

    /// Reads every remaining event.
    pub async fn collect(&self) -> Vec<T> {
        let mut events = Vec::new();
        while let Some(event) = self.next().await {
            events.push(event);
        }
        events
    }
}

/// The sender of an assistant response. It times the response: the final
/// message (of a `done` or `error` event, or passed to `end`) gets
/// `duration_ms`, measured monotonically from the sender's creation, unless
/// the message has one, its `timestamp` predates the sender, or the stream is
/// already done.
pub struct AssistantMessageEventSender {
    inner: EventSender<AssistantMessageEvent, AssistantMessage>,
    started_at: i64,
    started_at_monotonic: Instant,
}

/// The consumer of an assistant response.
pub type AssistantMessageEventStream = EventStream<AssistantMessageEvent, AssistantMessage>;

/// Creates a connected assistant response sender and stream.
pub fn assistant_message_channel() -> (AssistantMessageEventSender, AssistantMessageEventStream) {
    let (inner, stream) = event_channel(
        |event: &AssistantMessageEvent| {
            matches!(
                event,
                AssistantMessageEvent::Done { .. } | AssistantMessageEvent::Error { .. }
            )
        },
        |event: &AssistantMessageEvent| match event {
            AssistantMessageEvent::Done { message, .. } => Some(message.clone()),
            AssistantMessageEvent::Error { error, .. } => Some(error.clone()),
            _ => None,
        },
    );
    (
        AssistantMessageEventSender {
            inner,
            started_at: crate::now_ms(),
            started_at_monotonic: Instant::now(),
        },
        stream,
    )
}

impl AssistantMessageEventSender {
    /// Queues `event`, timing a final message.
    pub fn push(&self, mut event: AssistantMessageEvent) {
        match &mut event {
            AssistantMessageEvent::Done { message, .. } => self.time(message),
            AssistantMessageEvent::Error { error, .. } => self.time(error),
            _ => {}
        }
        self.inner.push(event);
    }

    /// Ends the stream, timing `result`.
    pub fn end(&self, result: Option<AssistantMessage>) {
        let result = result.map(|mut message| {
            self.time(&mut message);
            message
        });
        self.inner.end(result);
    }

    /// Whether every consumer is gone; a producer may stop its work.
    pub fn is_closed(&self) -> bool {
        self.inner.is_closed()
    }

    /// Pushes the terminal event for `message`: `done` for a completed stop
    /// reason, `error` otherwise; then ends the stream.
    pub fn finish(&self, message: AssistantMessage) {
        let reason = message.stop_reason;
        let event = match reason {
            crate::types::StopReason::Error | crate::types::StopReason::Aborted => {
                AssistantMessageEvent::Error {
                    reason,
                    error: message,
                }
            }
            _ => AssistantMessageEvent::Done { reason, message },
        };
        self.push(event);
        self.end(None);
    }

    fn time(&self, message: &mut AssistantMessage) {
        if self.inner.is_done()
            || message.duration_ms.is_some()
            || message.timestamp < self.started_at
        {
            return;
        }
        let elapsed = self.started_at_monotonic.elapsed().as_millis();
        message.duration_ms = Some(u64::try_from(elapsed).unwrap_or(u64::MAX));
    }
}

#[cfg(test)]
mod tests {
    //! Ports of Pi `packages/ai/test/event-stream.test.ts`.

    use super::*;
    use crate::types::{StopReason, Usage};
    use std::time::Duration;

    fn number_channel(
        complete_at: Option<i32>,
    ) -> (EventSender<i32, String>, EventStream<i32, String>) {
        if complete_at == Some(3) {
            event_channel(
                |event: &i32| *event == 3,
                |event: &i32| Some(event.to_string()),
            )
        } else {
            event_channel(|_: &i32| false, |event: &i32| Some(event.to_string()))
        }
    }

    // "drains buffered events in order and ignores events pushed after completion"
    #[tokio::test]
    async fn drains_buffered_events_and_ignores_pushes_after_completion() {
        let (sender, stream) = number_channel(Some(3));
        for event in 1..=4 {
            sender.push(event);
        }
        assert_eq!(stream.result().await.as_deref(), Some("3"));
        assert_eq!(stream.collect().await, vec![1, 2, 3]);
    }

    // "preserves order when events arrive after buffered draining starts"
    #[tokio::test]
    async fn preserves_order_when_events_arrive_while_draining() {
        let (sender, stream) = number_channel(None);
        sender.push(1);
        sender.push(2);
        assert_eq!(stream.next().await, Some(1));
        sender.push(3);
        assert_eq!(stream.next().await, Some(2));
        assert_eq!(stream.next().await, Some(3));
        sender.end(Some("3".into()));
        assert_eq!(stream.next().await, None);
    }

    // "delivers events to waiting consumers in registration order"
    #[tokio::test]
    async fn delivers_events_to_waiting_consumers() {
        let (sender, stream) = number_channel(None);
        let first = tokio::spawn({
            let stream = stream.clone();
            async move { stream.next().await }
        });
        let second = tokio::spawn({
            let stream = stream.clone();
            async move { stream.next().await }
        });
        tokio::time::sleep(Duration::from_millis(10)).await;
        sender.push(1);
        sender.push(2);
        let mut got = vec![first.await.unwrap(), second.await.unwrap()];
        got.sort();
        assert_eq!(got, vec![Some(1), Some(2)]);
    }

    // "drains buffered events after end and resolves the explicit result"
    #[tokio::test]
    async fn drains_after_end_with_explicit_result() {
        let (sender, stream) = number_channel(None);
        sender.push(1);
        sender.push(2);
        sender.end(Some("complete".into()));
        assert_eq!(stream.result().await.as_deref(), Some("complete"));
        assert_eq!(stream.collect().await, vec![1, 2]);
    }

    // "wakes all waiting consumers when ended without a result"
    #[tokio::test]
    async fn wakes_all_waiting_consumers_when_ended() {
        let (sender, stream) = number_channel(None);
        let first = tokio::spawn({
            let stream = stream.clone();
            async move { stream.next().await }
        });
        let second = tokio::spawn({
            let stream = stream.clone();
            async move { stream.next().await }
        });
        tokio::time::sleep(Duration::from_millis(10)).await;
        sender.end(None);
        assert_eq!(first.await.unwrap(), None);
        assert_eq!(second.await.unwrap(), None);
        assert_eq!(stream.result().await, None);
    }

    #[tokio::test]
    async fn dropping_the_sender_ends_the_stream() {
        let (sender, stream) = number_channel(None);
        sender.push(7);
        drop(sender);
        assert_eq!(stream.collect().await, vec![7]);
        assert_eq!(stream.result().await, None);
    }

    fn message(timestamp: i64, duration_ms: Option<u64>) -> AssistantMessage {
        AssistantMessage {
            content: Vec::new(),
            api: "openai-responses".into(),
            provider: "openai".into(),
            model: "m".into(),
            response_model: None,
            response_id: None,
            provider_thinking_level: None,
            thinking_level: None,
            diagnostics: None,
            usage: Usage::default(),
            stop_reason: StopReason::Stop,
            deferred: None,
            error_message: None,
            raw_stop_reason: None,
            end_turn: None,
            timestamp,
            duration_ms,
        }
    }

    fn done(message: AssistantMessage) -> AssistantMessageEvent {
        AssistantMessageEvent::Done {
            reason: StopReason::Stop,
            message,
        }
    }

    // "sets durationMs on the final done or error message of a response it saw start"
    #[tokio::test]
    async fn times_the_final_message() {
        let (sender, stream) = assistant_message_channel();
        let answer = message(crate::now_ms(), None);
        tokio::time::sleep(Duration::from_millis(20)).await;
        sender.push(done(answer));
        let result = stream.result().await.unwrap();
        assert!(result.duration_ms.unwrap() >= 15);

        let (failed, failed_stream) = assistant_message_channel();
        let mut error = message(crate::now_ms(), None);
        error.stop_reason = StopReason::Error;
        failed.push(AssistantMessageEvent::Error {
            reason: StopReason::Error,
            error,
        });
        assert!(failed_stream.result().await.unwrap().duration_ms.is_some());

        let (ended, ended_stream) = assistant_message_channel();
        ended.end(Some(message(crate::now_ms(), None)));
        assert!(ended_stream.result().await.unwrap().duration_ms.is_some());
    }

    // "keeps an existing duration, so a forwarding stream keeps the inner measurement"
    #[tokio::test]
    async fn keeps_an_existing_duration() {
        let (sender, stream) = assistant_message_channel();
        sender.push(done(message(crate::now_ms(), Some(1234))));
        assert_eq!(stream.result().await.unwrap().duration_ms, Some(1234));
    }

    // "leaves a message untimed when it started before the stream, such as a fetched deferred result"
    #[tokio::test]
    async fn leaves_messages_that_predate_the_stream_untimed() {
        let (sender, stream) = assistant_message_channel();
        sender.push(done(message(crate::now_ms() - 60_000, None)));
        assert_eq!(stream.result().await.unwrap().duration_ms, None);
    }

    // "does not time a message pushed after the stream completed"
    #[tokio::test]
    async fn ignores_messages_after_completion() {
        let (sender, stream) = assistant_message_channel();
        sender.push(done(message(crate::now_ms(), None)));
        sender.push(done(message(crate::now_ms(), None)));
        let events = stream.collect().await;
        assert_eq!(events.len(), 1);
    }
}
