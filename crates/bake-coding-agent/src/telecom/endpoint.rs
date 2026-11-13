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
//! hostile peer costs one bounded task.

use std::path::Path;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

use super::wire::{FRAME_CAP, Frame, Outcome, Response, WIRE_VERSION};
use crate::owned_work::TaskScope;

/// How long either side of a delivery waits for the other.
pub const IO_TIMEOUT: Duration = Duration::from_secs(5);

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
    /// progress (each bounded by [`IO_TIMEOUT`]).
    pub async fn serve(self, handler: Handler, scope: TaskScope) {
        let mut connections = tokio::task::JoinSet::new();
        let slots = std::sync::Arc::new(tokio::sync::Semaphore::new(CONCURRENT));
        loop {
            tokio::select! {
                () = scope.signal().cancelled() => break,
                Some(_) = connections.join_next(), if !connections.is_empty() => {}
                accepted = self.0.accept() => {
                    let Ok((stream, _)) = accepted else { continue };
                    let Ok(slot) = std::sync::Arc::clone(&slots).acquire_owned().await else {
                        break;
                    };
                    let handler = std::sync::Arc::clone(&handler);
                    connections.spawn(async move {
                        let _slot = slot;
                        let _ = tokio::time::timeout(IO_TIMEOUT, answer(stream, handler)).await;
                    });
                }
            }
        }
        while connections.join_next().await.is_some() {}
    }
}

#[cfg(unix)]
async fn answer(mut stream: tokio::net::UnixStream, handler: Handler) {
    let line = match read_line(&mut stream).await {
        Ok(line) => line,
        Err(_) => return,
    };
    let (id, outcome) = match Frame::decode(&line) {
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
                Outcome::Accepted
            })
        };
        let gate = WorkGate::default();
        let listener = Listener::bind(&socket).unwrap();
        let _task = gate
            .spawn(move |scope| listener.serve(handler, scope))
            .unwrap();

        assert_eq!(deliver(&socket, &frame("hi")).await, Ok(Outcome::Accepted));
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
}
