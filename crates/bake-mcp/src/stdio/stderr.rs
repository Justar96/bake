//! The server's standard error: read by a task the process owner owns,
//! keeping only a bounded tail, as Pi keeps `maxStderrBytes` (64 KiB).

use std::io;
use std::sync::{Arc, Mutex, PoisonError};

use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::sync::watch;

const READ_CHUNK: usize = 8 * 1024;

/// What has been read from the server's standard error.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StderrSnapshot {
    /// The last bytes read, at most the configured limit. They may start
    /// inside a UTF-8 character; decode lossily.
    pub tail: Vec<u8>,
    /// Every byte read, including those the tail no longer holds.
    pub total_bytes: u64,
    /// Whether the stream has ended (every holder of the pipe closed it).
    pub ended: bool,
    /// Why reading stopped early, if it did. Once reading stops, the read end
    /// is closed, and the server's next write to standard error fails
    /// (`EPIPE`, or `SIGPIPE` if the server does not handle it).
    pub error: Option<(io::ErrorKind, String)>,
}

impl StderrSnapshot {
    /// The tail decoded lossily.
    pub fn tail_text(&self) -> String {
        String::from_utf8_lossy(&self.tail).into_owned()
    }

    /// Whether earlier bytes were dropped from the tail.
    pub fn truncated(&self) -> bool {
        self.total_bytes > self.tail.len() as u64
    }
}

/// The reader's shared state; the watch carries `total_bytes`, so a waiter
/// wakes on every chunk and on the end.
#[derive(Debug)]
pub(super) struct StderrState {
    snapshot: Mutex<StderrSnapshot>,
    limit: usize,
    updates: watch::Sender<u64>,
}

impl StderrState {
    pub(super) fn new(limit: usize) -> Arc<Self> {
        Arc::new(Self {
            snapshot: Mutex::new(StderrSnapshot::default()),
            limit,
            updates: watch::channel(0).0,
        })
    }

    pub(super) fn snapshot(&self) -> StderrSnapshot {
        self.snapshot
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    pub(super) fn subscribe(&self) -> watch::Receiver<u64> {
        self.updates.subscribe()
    }

    fn update(&self, change: impl FnOnce(&mut StderrSnapshot)) {
        let total = {
            let mut snapshot = self.snapshot.lock().unwrap_or_else(PoisonError::into_inner);
            change(&mut snapshot);
            snapshot.total_bytes
        };
        // `send_replace` notifies even when the total is unchanged (the end).
        self.updates.send_replace(total);
    }

    fn append(&self, bytes: &[u8]) {
        let limit = self.limit;
        self.update(|snapshot| {
            snapshot.total_bytes = snapshot.total_bytes.saturating_add(bytes.len() as u64);
            let keep = bytes.len().min(limit);
            snapshot
                .tail
                .extend_from_slice(&bytes[bytes.len() - keep..]);
            let excess = snapshot.tail.len().saturating_sub(limit);
            snapshot.tail.drain(..excess);
        });
    }
}

/// Read `reader` to its end into `state`. Memory stays within the tail limit
/// plus one chunk however much the server writes.
pub(super) async fn read<R: AsyncRead + Unpin>(mut reader: R, state: Arc<StderrState>) {
    let mut chunk = vec![0_u8; READ_CHUNK];
    loop {
        match reader.read(&mut chunk).await {
            Ok(0) => {
                state.update(|snapshot| snapshot.ended = true);
                return;
            }
            Ok(read) => state.append(&chunk[..read]),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => {
                state.update(|snapshot| snapshot.error = Some((error.kind(), error.to_string())));
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::pin::Pin;
    use std::task::{Context, Poll};

    use super::*;

    #[tokio::test]
    async fn keeps_a_bounded_tail_of_any_amount() {
        let state = StderrState::new(16);
        let input: Vec<u8> = (0..100_000_u32).map(|index| (index % 251) as u8).collect();
        read(input.as_slice(), state.clone()).await;
        let snapshot = state.snapshot();
        assert_eq!(snapshot.total_bytes, 100_000);
        assert_eq!(snapshot.tail, input[input.len() - 16..]);
        assert!(snapshot.ended && snapshot.truncated() && snapshot.error.is_none());
        // Control: a short stream is kept whole.
        let state = StderrState::new(16);
        read(&b"ready\n"[..], state.clone()).await;
        assert_eq!(state.snapshot().tail_text(), "ready\n");
        assert!(!state.snapshot().truncated());
    }

    /// Yields `data`, then fails.
    struct Failing(&'static [u8]);

    impl AsyncRead for Failing {
        fn poll_read(
            mut self: Pin<&mut Self>,
            _: &mut Context<'_>,
            buf: &mut tokio::io::ReadBuf<'_>,
        ) -> Poll<io::Result<()>> {
            if self.0.is_empty() {
                return Poll::Ready(Err(io::Error::other("stderr pipe broke")));
            }
            buf.put_slice(self.0);
            self.0 = &[];
            Poll::Ready(Ok(()))
        }
    }

    #[tokio::test]
    async fn a_failed_read_is_recorded_and_ends_the_reader() {
        let state = StderrState::new(64);
        let mut updates = state.subscribe();
        read(Failing(b"partial"), state.clone()).await;
        let snapshot = state.snapshot();
        assert_eq!(snapshot.tail_text(), "partial");
        assert!(!snapshot.ended);
        assert_eq!(
            snapshot.error,
            Some((io::ErrorKind::Other, "stderr pipe broke".into()))
        );
        assert!(updates.has_changed().unwrap());
        assert_eq!(*updates.borrow_and_update(), 7);
    }
}
