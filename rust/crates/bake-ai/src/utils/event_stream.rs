//! An asynchronous stream of events with a final result.
//!
//! Ported from Pi `packages/ai/src/utils/event-stream.ts` (v1.1.0). Pi's one
//! `EventStream` object both receives pushes and is iterated; here the
//! producer half is an [`EventSender`] and the consumer half an
//! [`EventStream`]. Dropping the sender ends the stream, so a producer task
//! that fails or is cancelled cannot leave a consumer waiting. Consumers read
//! with [`EventStream::next`] and await the final result with
//! [`EventStream::result`], which is `None` when the stream ended without one.
//!
//! Bake adds the producer's lifetime, which Pi's single-threaded event loop
//! does not need. The sender, and every [`ProducerGuard`] it hands out,
//! count as the live producer; [`EventStream::producer_ended`] and
//! [`ProducerWatch::ended`] complete once all of them are dropped. A producer
//! task holds a guard as the last thing it drops, so a consumer that stops
//! reading can await the task's own cleanup instead of detaching it.
//! [`EventSender::closed`] is the other direction: it completes once every
//! [`EventStream`] is gone, so a producer that is waiting on its source can
//! stop. A [`ProducerWatch`] is not a consumer: a caller that drops its
//! streams and keeps a watch both closes the stream and awaits the
//! producer.

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
    /// Live [`EventStream`] handles.
    consumers: usize,
    /// The live sender and [`ProducerGuard`]s.
    producers: usize,
    closed_wakers: Vec<Waker>,
    producer_wakers: Vec<Waker>,
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
            consumers: 1,
            producers: 1,
            closed_wakers: Vec::new(),
            producer_wakers: Vec::new(),
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
        self.shared.lock().consumers == 0
    }

    /// Completes once every consumer is gone; at once if they already are.
    pub async fn closed(&self) {
        poll_fn(|cx| {
            let mut inner = self.shared.lock();
            if inner.consumers == 0 {
                return Poll::Ready(());
            }
            register(&mut inner.closed_wakers, cx.waker());
            Poll::Pending
        })
        .await;
    }

    /// A token that counts as a live producer until dropped, so
    /// [`EventStream::producer_ended`] also waits for whatever its holder
    /// drops before it. It cannot push.
    pub fn producer_guard(&self) -> ProducerGuard
    where
        T: Send + 'static,
        R: Send + 'static,
    {
        self.shared.lock().producers += 1;
        ProducerGuard {
            release: Arc::clone(&self.shared) as Arc<dyn ReleaseProducer>,
        }
    }
}

impl<T, R> Drop for EventSender<T, R> {
    fn drop(&mut self) {
        self.end(None);
        self.shared.release_producer();
    }
}

/// Type-erased release of one producer count.
trait ReleaseProducer: Send + Sync {
    fn release_producer(&self);
}

impl<T: Send, R: Send> ReleaseProducer for Shared<T, R> {
    fn release_producer(&self) {
        Shared::release_producer(self);
    }
}

impl<T, R> Shared<T, R> {
    fn release_producer(&self) {
        let wakers = {
            let mut inner = self.lock();
            inner.producers -= 1;
            if inner.producers == 0 {
                std::mem::take(&mut inner.producer_wakers)
            } else {
                Vec::new()
            }
        };
        wakers.into_iter().for_each(Waker::wake);
    }
}

/// Counts as a live producer of one stream until dropped
/// ([`EventSender::producer_guard`]).
#[must_use = "a dropped guard no longer holds the producer open"]
pub struct ProducerGuard {
    release: Arc<dyn ReleaseProducer>,
}

impl Drop for ProducerGuard {
    fn drop(&mut self) {
        self.release.release_producer();
    }
}

impl std::fmt::Debug for ProducerGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProducerGuard").finish_non_exhaustive()
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
        self.shared.lock().consumers += 1;
        Self {
            shared: Arc::clone(&self.shared),
        }
    }
}

impl<T, R> Drop for EventStream<T, R> {
    fn drop(&mut self) {
        let wakers = {
            let mut inner = self.shared.lock();
            inner.consumers -= 1;
            if inner.consumers == 0 {
                std::mem::take(&mut inner.closed_wakers)
            } else {
                Vec::new()
            }
        };
        wakers.into_iter().for_each(Waker::wake);
    }
}

impl<T, R> EventStream<T, R> {
    /// Completes once the producer has ended: its sender and every
    /// [`ProducerGuard`] are dropped. This stream is itself a consumer, so
    /// while it is awaited [`EventSender::closed`] cannot fire: a producer
    /// that waits for its consumers to leave never ends here. Stop such a
    /// producer some other way (its abort signal), or take a
    /// [`Self::producer_watch`], drop every stream, and await the watch.
    pub async fn producer_ended(&self) {
        poll_fn(|cx| {
            let mut inner = self.shared.lock();
            if inner.producers == 0 {
                return Poll::Ready(());
            }
            register(&mut inner.producer_wakers, cx.waker());
            Poll::Pending
        })
        .await;
    }

    /// A handle that awaits the producer's end without being a consumer.
    pub fn producer_watch(&self) -> ProducerWatch
    where
        T: Send + 'static,
        R: Send + 'static,
    {
        ProducerWatch {
            shared: Arc::clone(&self.shared) as Arc<dyn ProducerState>,
        }
    }
}

/// Type-erased view of a stream's producer count.
trait ProducerState: Send + Sync {
    fn poll_ended(&self, waker: &Waker) -> Poll<()>;
}

impl<T: Send, R: Send> ProducerState for Shared<T, R> {
    fn poll_ended(&self, waker: &Waker) -> Poll<()> {
        let mut inner = self.lock();
        if inner.producers == 0 {
            return Poll::Ready(());
        }
        register(&mut inner.producer_wakers, waker);
        Poll::Pending
    }
}

/// Awaits a stream's producer ([`EventStream::producer_watch`]) without
/// counting as a consumer, so [`EventSender::closed`] fires once every
/// [`EventStream`] is dropped even while this is held or awaited.
pub struct ProducerWatch {
    shared: Arc<dyn ProducerState>,
}

impl ProducerWatch {
    /// Completes once the sender and every [`ProducerGuard`] are dropped.
    pub async fn ended(&self) {
        poll_fn(|cx| self.shared.poll_ended(cx.waker())).await;
    }
}

impl std::fmt::Debug for ProducerWatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProducerWatch").finish_non_exhaustive()
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

    /// [`EventSender::closed`].
    pub async fn closed(&self) {
        self.inner.closed().await;
    }

    /// [`EventSender::producer_guard`].
    pub fn producer_guard(&self) -> ProducerGuard {
        self.inner.producer_guard()
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

    fn poll_once<F: std::future::Future + Unpin>(future: &mut F) -> Poll<F::Output> {
        let mut cx = std::task::Context::from_waker(Waker::noop());
        std::pin::Pin::new(future).poll(&mut cx)
    }

    // Bake: `closed` completes only once every consumer, clones included,
    // is gone, and wakes a producer already waiting on it.
    #[tokio::test]
    async fn closed_waits_for_every_consumer() {
        let (sender, stream) = number_channel(None);
        let clone = stream.clone();
        let mut closed = Box::pin(sender.closed());
        assert!(poll_once(&mut closed).is_pending());
        drop(stream);
        assert!(poll_once(&mut closed).is_pending(), "a clone still reads");
        assert!(!sender.is_closed());
        drop(clone);
        assert!(sender.is_closed());
        tokio::time::timeout(Duration::from_secs(10), closed)
            .await
            .expect("closed wakes its waiter");
    }

    // Bake: `producer_ended` waits for the sender and every guard, not for
    // the end of the events: a producer's cleanup after its last event is
    // covered.
    #[tokio::test]
    async fn producer_ended_waits_for_the_sender_and_its_guards() {
        let (sender, stream) = number_channel(None);
        let guard = sender.producer_guard();
        sender.end(Some("done".into()));
        let mut ended = Box::pin(stream.producer_ended());
        assert!(poll_once(&mut ended).is_pending(), "the sender is alive");
        drop(sender);
        assert_eq!(stream.result().await.as_deref(), Some("done"));
        assert!(poll_once(&mut ended).is_pending(), "a guard is alive");
        drop(guard);
        tokio::time::timeout(Duration::from_secs(10), ended)
            .await
            .expect("producer_ended wakes its waiter");
    }

    // Bake: a watch is not a consumer. A producer that waits for its
    // consumers to leave ends once the last stream is dropped, and the
    // watch sees it; the stream's own `producer_ended` could not, since
    // that stream keeps `closed` from firing (the review's finding).
    #[tokio::test]
    async fn a_watch_awaits_a_producer_that_waits_for_its_consumers() {
        let (sender, stream) = number_channel(None);
        let guard = sender.producer_guard();
        let (closed_tx, closed_rx) = tokio::sync::oneshot::channel();
        let producer = tokio::spawn(async move {
            sender.closed().await;
            let _ = closed_tx.send(());
            drop(sender);
            drop(guard);
        });
        let watch = stream.producer_watch();
        let mut by_stream = Box::pin(stream.producer_ended());
        assert!(poll_once(&mut by_stream).is_pending());
        drop(by_stream);
        drop(stream);
        tokio::time::timeout(Duration::from_secs(10), closed_rx)
            .await
            .expect("the producer sees its consumers leave")
            .expect("sent");
        tokio::time::timeout(Duration::from_secs(10), watch.ended())
            .await
            .expect("the watch sees the producer end");
        producer.await.expect("joined");
    }
}
