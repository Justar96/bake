//! Bake: a spawned provider task's lifetime is observable through its
//! stream ([`spawn_stream`]), so a consumer that stops reading can await
//! the task's own cleanup instead of detaching it.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use super::*;

struct Cleanup(Arc<AtomicBool>);

impl Drop for Cleanup {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

// The task drops its sender first, as a provider does once it has
// finished the stream, then holds its cleanup; `producer_ended` waits for
// the cleanup, and a stream whose consumer is gone still ends it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn producer_ended_covers_the_task_after_its_sender() {
    let faux = crate::providers::faux::FauxProvider::new(Default::default());
    let model = faux.model().clone();
    let cleaned = Arc::new(AtomicBool::new(false));
    let (release, released) = tokio::sync::oneshot::channel::<()>();
    let (sent_tx, sent) = tokio::sync::oneshot::channel::<()>();
    let flag = Arc::clone(&cleaned);
    let stream = spawn_stream(&model, move |sender| async move {
        let _cleanup = Cleanup(flag);
        sender.end(None);
        drop(sender);
        let _ = sent_tx.send(());
        let _ = released.await;
    });
    tokio::time::timeout(Duration::from_secs(10), sent)
        .await
        .expect("the task runs")
        .expect("sent");
    assert_eq!(stream.result().await, None);
    let mut ended = Box::pin(stream.producer_ended());
    let early = tokio::time::timeout(Duration::from_millis(200), &mut ended)
        .await
        .is_ok();
    let cleaned_early = cleaned.load(Ordering::SeqCst);
    release.send(()).expect("the task waits");
    if !early {
        tokio::time::timeout(Duration::from_secs(10), ended)
            .await
            .expect("producer_ended finishes");
    }
    assert!(
        !early,
        "producer_ended finished while the task held cleanup"
    );
    assert!(!cleaned_early);
    assert!(cleaned.load(Ordering::SeqCst));
}
