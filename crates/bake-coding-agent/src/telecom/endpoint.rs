//! The socket a member's frames arrive on, and the client that delivers
//! one.
//!
//! A delivery is one connection: the sender writes one [`Frame`] line, the
//! member answers one [`Response`] line and closes. The answer comes after
//! the member's session has taken the frame (`accepted`) or declined it
//! (`refused`), so a sender knows which recipients have it and which do
//! not; a connection refused, a timeout, or a malformed answer is a
//! failed delivery, never a silent one. Both sides read at most
//! [`FRAME_CAP`] bytes and give up after [`IO_TIMEOUT`], so a stalled or
//! hostile peer costs one bounded task. A frame sent longer ago than that
//! is refused unread, since its sender has stopped waiting and counts it
//! undelivered. The server reads a bounded number of connections at once;
//! the rest wait inside their own time limit, never in the accept loop, so
//! cancellation stops the server at once.

use std::path::Path;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

use super::wire::{FRAME_CAP, Frame, Outcome, Response, WIRE_VERSION};
use crate::owned_work::TaskScope;
use bake_ai::AbortSignal;

/// How long either side of a delivery waits for the other.
pub const IO_TIMEOUT: Duration = Duration::from_secs(5);

/// How long the server pauses after a failed accept.
const ACCEPT_BACKOFF: Duration = Duration::from_millis(100);

/// The most deliveries a member reads at once; more wait to be accepted.
const CONCURRENT: usize = 16;

/// Takes an arriving frame: returns what to answer.
pub type Handler = std::sync::Arc<dyn Fn(Frame) -> Outcome + Send + Sync>;

/// Reads one line of at most [`FRAME_CAP`] bytes, its line feed removed.
async fn read_line<R: tokio::io::AsyncRead + Unpin>(reader: R) -> Result<Vec<u8>, String> {
    let mut line = Vec::new();
    let mut limited = BufReader::new(reader).take(FRAME_CAP as u64 + 1);
    limited
        .read_until(b'\n', &mut line)
        .await
        .map_err(|error| error.to_string())?;
    match line.last() {
        Some(b'\n') => {
            line.pop();
            Ok(line)
        }
        _ if line.len() > FRAME_CAP => Err("line too long".to_owned()),
        _ => Err("connection closed before a full line".to_owned()),
    }
}

/// Whether a frame sent at `sent_at` (Unix milliseconds, this machine's
/// clock) is older than its sender's wait for an answer.
fn is_stale(sent_at: i64) -> bool {
    let limit = i64::try_from(IO_TIMEOUT.as_millis()).unwrap_or(i64::MAX);
    bake_ai::now_ms().saturating_sub(sent_at) > limit
}

/// Delivers `frame` to the member listening on `socket`.
#[cfg(unix)]
pub async fn deliver(socket: &Path, frame: &Frame) -> Result<Outcome, String> {
    let exchange = async {
        let mut stream = tokio::net::UnixStream::connect(socket)
            .await
            .map_err(|error| format!("cannot connect: {error}"))?;
        stream
            .write_all(&frame.encode())
            .await
            .map_err(|error| format!("cannot write: {error}"))?;
        let line = read_line(&mut stream).await?;
        Response::decode(&line, &frame.id).map_err(|error| error.to_string())
    };
    tokio::time::timeout(IO_TIMEOUT, exchange)
        .await
        .unwrap_or_else(|_| Err("no answer within 5 s".to_owned()))
}

#[cfg(not(unix))]
pub async fn deliver(_socket: &Path, _frame: &Frame) -> Result<Outcome, String> {
    Err(super::UNSUPPORTED.to_owned())
}

/// A bound socket, not yet serving.
#[cfg(unix)]
pub struct Listener(tokio::net::UnixListener);

#[cfg(unix)]
impl Listener {
    /// Binds `socket`, private to this user. The caller holds the lock
    /// that makes the path its own and has cleared any stale file there.
    pub fn bind(socket: &Path) -> std::io::Result<Self> {
        let listener = tokio::net::UnixListener::bind(socket)?;
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(socket, std::fs::Permissions::from_mode(0o600))?;
        Ok(Self(listener))
    }

    /// Serves until `scope` is cancelled, then awaits the deliveries in
    /// progress: one whose frame is unread ends at once, one whose frame
    /// was taken finishes its answer (each bounded by [`IO_TIMEOUT`]).
    pub async fn serve(self, handler: Handler, scope: TaskScope) {
        let mut connections = tokio::task::JoinSet::new();
        let slots = std::sync::Arc::new(tokio::sync::Semaphore::new(CONCURRENT));
        loop {
            tokio::select! {
                biased;
                () = scope.signal().cancelled() => break,
                Some(_) = connections.join_next(), if !connections.is_empty() => {}
                accepted = self.0.accept() => {
                    let Ok((stream, _)) = accepted else {
                        // A failed accept (out of descriptors, say) is
                        // tried again after a pause, not at once.
                        tokio::select! {
                            biased;
                            () = scope.signal().cancelled() => break,
                            () = tokio::time::sleep(ACCEPT_BACKOFF) => continue,
                        }
                    };
                    let slots = std::sync::Arc::clone(&slots);
                    let handler = std::sync::Arc::clone(&handler);
                    let signal = scope.signal().clone();
                    connections.spawn(async move {
                        // The wait for a slot counts against the delivery's
                        // time, so a frame that waited past its sender's
                        // patience is refused as stale, not taken late.
                        let _ = tokio::time::timeout(IO_TIMEOUT, async {
                            let slot = tokio::select! {
                                biased;
                                () = signal.cancelled() => return,
                                slot = slots.acquire_owned() => slot,
                            };
                            let Ok(_slot) = slot else { return };
                            answer(stream, handler, &signal).await;
                        })
                        .await;
                    });
                }
            }
        }
        while connections.join_next().await.is_some() {}
    }
}

#[cfg(unix)]
async fn answer(mut stream: tokio::net::UnixStream, handler: Handler, signal: &AbortSignal) {
    // Until the frame is read nothing is taken, so a cancelled server
    // drops the connection, which its sender counts as undelivered.
    let line = tokio::select! {
        biased;
        () = signal.cancelled() => return,
        line = read_line(&mut stream) => match line {
            Ok(line) => line,
            Err(_) => return,
        },
    };
    let (id, outcome) = match Frame::decode(&line) {
        Ok(frame) if is_stale(frame.sent_at) => (
            frame.id,
            Outcome::Refused {
                reason: format!(
                    "sent more than {} s ago; its sender has stopped waiting",
                    IO_TIMEOUT.as_secs()
                ),
            },
        ),
        Ok(frame) => (frame.id.clone(), handler(frame)),
        Err(error) => {
            // Answer under the frame's id when it carries a usable one, so
            // its sender learns why.
            let id = serde_json::from_slice::<serde_json::Value>(&line)
                .ok()
                .and_then(|value| value.get("id")?.as_str().map(str::to_owned))
                .filter(|id| super::wire::is_id(id));
            let Some(id) = id else { return };
            (
                id,
                Outcome::Refused {
                    reason: error.to_string(),
                },
            )
        }
    };
    let response = Response {
        v: WIRE_VERSION,
        id,
        outcome,
    };
    let _ = stream.write_all(&response.encode()).await;
    let _ = stream.shutdown().await;
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::owned_work::WorkGate;
    use crate::telecom::wire::{Body, Sender};
    use std::sync::{Arc, Mutex};

    fn frame(text: &str) -> Frame {
        Frame::new(
            Sender {
                session: "s".into(),
                callsign: "alpha".into(),
            },
            Body::Message {
                text: text.into(),
                group: Vec::new(),
                human: false,
            },
        )
    }

    #[tokio::test]
    async fn a_delivery_is_answered_after_the_handler_takes_it() {
        let dir = std::env::temp_dir().join(format!("bt-{}", crate::session::id::uuid_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let socket = dir.join("bravo.sock");
        let seen = Arc::new(Mutex::new(Vec::new()));
        let handler: Handler = {
            let seen = Arc::clone(&seen);
            Arc::new(move |frame: Frame| {
                let Body::Message { text, .. } = frame.body else {
                    unreachable!()
                };
                if text == "no" {
                    return Outcome::Refused {
                        reason: "busy".into(),
                    };
                }
                seen.lock().unwrap().push(text);
                Outcome::Accepted { held: false }
            })
        };
        let gate = WorkGate::default();
        let listener = Listener::bind(&socket).unwrap();
        let _task = gate
            .spawn(move |scope| listener.serve(handler, scope))
            .unwrap();

        assert_eq!(
            deliver(&socket, &frame("hi")).await,
            Ok(Outcome::Accepted { held: false })
        );
        assert_eq!(
            deliver(&socket, &frame("no")).await,
            Ok(Outcome::Refused {
                reason: "busy".into()
            })
        );
        assert_eq!(*seen.lock().unwrap(), ["hi"]);

        // A malformed frame with a usable id is refused with the reason.
        let mut stream = tokio::net::UnixStream::connect(&socket).await.unwrap();
        stream
            .write_all(b"{\"v\":1,\"id\":\"f9\",\"from\":{}}\n")
            .await
            .unwrap();
        let line = read_line(&mut stream).await.unwrap();
        assert!(matches!(
            Response::decode(&line, "f9"),
            Ok(Outcome::Refused { .. })
        ));

        gate.close().await;
        assert!(deliver(&socket, &frame("late")).await.is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn a_frame_older_than_its_senders_wait_is_refused_unread() {
        let dir = std::env::temp_dir().join(format!("bt-{}", crate::session::id::uuid_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let socket = dir.join("bravo.sock");
        let seen = Arc::new(Mutex::new(0));
        let handler: Handler = {
            let seen = Arc::clone(&seen);
            Arc::new(move |_frame: Frame| {
                *seen.lock().unwrap() += 1;
                Outcome::Accepted { held: false }
            })
        };
        let gate = WorkGate::default();
        let listener = Listener::bind(&socket).unwrap();
        let _task = gate
            .spawn(move |scope| listener.serve(handler, scope))
            .unwrap();

        let mut late = frame("from a sender that gave up");
        late.sent_at -= i64::try_from(IO_TIMEOUT.as_millis()).unwrap() + 1000;
        let outcome = deliver(&socket, &late).await.unwrap();
        assert!(
            matches!(&outcome, Outcome::Refused { reason } if reason.contains("stopped waiting")),
            "{outcome:?}"
        );
        assert_eq!(*seen.lock().unwrap(), 0);
        assert_eq!(
            deliver(&socket, &frame("fresh")).await,
            Ok(Outcome::Accepted { held: false })
        );
        assert_eq!(*seen.lock().unwrap(), 1);

        gate.close().await;
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn a_cancelled_server_stops_without_waiting_on_idle_connections() {
        let dir = std::env::temp_dir().join(format!("bt-{}", crate::session::id::uuid_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let socket = dir.join("bravo.sock");
        let handler: Handler = Arc::new(|_frame: Frame| Outcome::Accepted { held: false });
        let gate = WorkGate::default();
        let listener = Listener::bind(&socket).unwrap();
        let _task = gate
            .spawn(move |scope| listener.serve(handler, scope))
            .unwrap();
        assert_eq!(
            deliver(&socket, &frame("hi")).await,
            Ok(Outcome::Accepted { held: false })
        );
        // More silent connections than the server reads at once: every
        // slot is taken and the rest wait for one.
        let mut idle = Vec::new();
        for _ in 0..CONCURRENT + 2 {
            idle.push(tokio::net::UnixStream::connect(&socket).await.unwrap());
        }
        tokio::task::yield_now().await;
        // The property is the bound: a silent peer must not hold the
        // session's close for its read timeout.
        let started = std::time::Instant::now();
        gate.close().await;
        assert!(started.elapsed() < IO_TIMEOUT, "{:?}", started.elapsed());
        for mut stream in idle {
            let mut rest = Vec::new();
            assert_eq!(stream.read_to_end(&mut rest).await.unwrap(), 0);
        }
        std::fs::remove_dir_all(dir).unwrap();
    }
}
