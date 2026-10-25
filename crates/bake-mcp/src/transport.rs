//! The message transport a client runs over, and newline-delimited framing
//! for byte streams.
//!
//! Pi's `McpTransport` (`packages/mcp/src/transports/transport.ts`, v1.3.0)
//! starts, sends, closes, and pushes messages to listeners. Here a transport
//! is two halves the client's driver task owns: a [`MessageReceiver`] it pulls
//! whole messages from and a [`MessageSender`] it writes them to. Pulling
//! instead of pushing lets the driver stop reading while its outgoing queue is
//! full, which bounds memory without dropping messages.
//!
//! [`LineReader`] and [`LineWriter`] frame messages as the stdio transport
//! does (`src/transports/stdio.ts`): one JSON-RPC message per line, UTF-8,
//! with no embedded newline. They work over any Tokio byte stream; spawning
//! the server process, its environment, sandbox, and process-tree shutdown
//! belong to the caller that owns the process.

use std::fmt;
use std::future::Future;
use std::io;
use std::num::NonZeroUsize;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// The default limit of one message's bytes, as in Pi
/// (`DEFAULT_MAX_MESSAGE_BYTES`, 16 MiB).
pub const DEFAULT_MAX_MESSAGE_BYTES: usize = 16 * 1024 * 1024;

const READ_CHUNK: usize = 8 * 1024;

/// The receiving half of a transport.
pub trait MessageReceiver: Send + 'static {
    /// The next whole message's bytes, or `None` once the peer has ended the
    /// stream cleanly.
    ///
    /// The returned future must be cancel-safe: the client polls it in a
    /// `select!` and drops it whenever another event comes first, so dropping
    /// it must lose no bytes. After an error the receiver is not used again.
    fn receive(&mut self) -> impl Future<Output = Result<Option<Vec<u8>>, ReceiveError>> + Send;
}

/// The sending half of a transport.
pub trait MessageSender: Send + 'static {
    /// Write one whole message. The client runs one send at a time, in queue
    /// order, and does not drop a send it started except when a shutdown's
    /// deadline passes.
    fn send(&mut self, message: &[u8]) -> impl Future<Output = io::Result<()>> + Send;

    /// Flush and end the stream, telling the server no more messages come.
    /// The client calls it once, at shutdown, under the shutdown deadline.
    fn close(&mut self) -> impl Future<Output = io::Result<()>> + Send;
}

/// Why a [`MessageReceiver`] stopped.
#[derive(Debug)]
#[non_exhaustive]
pub enum ReceiveError {
    /// A message exceeded the limit. Newline framing cannot resynchronize
    /// safely inside an unbounded line, so the stream is not read further.
    TooLarge {
        /// The limit in bytes.
        limit: usize,
    },
    /// The stream ended inside a message.
    Incomplete {
        /// The bytes of the unfinished message.
        bytes: usize,
    },
    /// Reading failed.
    Io(io::Error),
}

impl fmt::Display for ReceiveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooLarge { limit } => write!(f, "an MCP message exceeds {limit} bytes"),
            Self::Incomplete { bytes } => write!(
                f,
                "the MCP server's output ended inside a message ({bytes} bytes without a newline)"
            ),
            Self::Io(error) => write!(f, "reading from the MCP server failed: {error}"),
        }
    }
}

impl std::error::Error for ReceiveError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            _ => None,
        }
    }
}

/// Reads newline-delimited messages from a byte stream, holding at most one
/// message plus one read chunk (8 KiB) in memory.
///
/// A line is at most `limit` bytes, counted before its `\n` (a `\r` before
/// it counts, then is stripped as Pi strips it). A longer line fails with
/// [`ReceiveError::TooLarge`] as soon as the limit is passed, without waiting
/// for its end. Blank lines are returned as they are; the client skips them.
#[derive(Debug)]
pub struct LineReader<R> {
    inner: R,
    buffer: Vec<u8>,
    /// Bytes of `buffer` already searched for a newline.
    scanned: usize,
    limit: usize,
    state: ReadState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReadState {
    Open,
    Ended,
    /// Fused after an oversized line: the rest of it is never read.
    TooLarge,
}

impl<R> LineReader<R> {
    /// Frame `inner`, refusing lines over `limit` bytes.
    pub fn new(inner: R, limit: NonZeroUsize) -> Self {
        Self {
            inner,
            buffer: Vec::new(),
            scanned: 0,
            limit: limit.get(),
            state: ReadState::Open,
        }
    }

    /// The wrapped stream.
    pub fn into_inner(self) -> R {
        self.inner
    }

    fn take_line(&mut self) -> Option<Result<Vec<u8>, ReceiveError>> {
        let offset = self.buffer[self.scanned..]
            .iter()
            .position(|byte| *byte == b'\n')?;
        let end = self.scanned + offset;
        let rest = self.buffer.split_off(end + 1);
        let mut line = std::mem::replace(&mut self.buffer, rest);
        self.scanned = 0;
        line.pop();
        if line.len() > self.limit {
            self.state = ReadState::TooLarge;
            return Some(Err(ReceiveError::TooLarge { limit: self.limit }));
        }
        if line.last() == Some(&b'\r') {
            line.pop();
        }
        Some(Ok(line))
    }
}

impl<R: AsyncRead + Unpin + Send + 'static> MessageReceiver for LineReader<R> {
    async fn receive(&mut self) -> Result<Option<Vec<u8>>, ReceiveError> {
        loop {
            match self.state {
                ReadState::TooLarge => return Err(ReceiveError::TooLarge { limit: self.limit }),
                ReadState::Ended => {
                    if let Some(line) = self.take_line() {
                        return line.map(Some);
                    }
                    return match self.buffer.len() {
                        0 => Ok(None),
                        bytes => Err(ReceiveError::Incomplete { bytes }),
                    };
                }
                ReadState::Open => {}
            }
            if let Some(line) = self.take_line() {
                return line.map(Some);
            }
            self.scanned = self.buffer.len();
            if self.buffer.len() > self.limit {
                self.state = ReadState::TooLarge;
                self.buffer = Vec::new();
                return Err(ReceiveError::TooLarge { limit: self.limit });
            }
            // `read` is cancel-safe: if this future is dropped while waiting,
            // no bytes were taken from the stream, and bytes it did take are
            // appended before the next await.
            let mut chunk = [0_u8; READ_CHUNK];
            match self.inner.read(&mut chunk).await {
                Ok(0) => self.state = ReadState::Ended,
                Ok(read) => self.buffer.extend_from_slice(&chunk[..read]),
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => return Err(ReceiveError::Io(error)),
            }
        }
    }
}

/// Writes each message followed by `\n`, and flushes it.
#[derive(Debug)]
pub struct LineWriter<W> {
    inner: W,
}

impl<W> LineWriter<W> {
    /// Frame `inner`.
    pub fn new(inner: W) -> Self {
        Self { inner }
    }

    /// The wrapped stream.
    pub fn into_inner(self) -> W {
        self.inner
    }
}

impl<W: AsyncWrite + Unpin + Send + 'static> MessageSender for LineWriter<W> {
    async fn send(&mut self, message: &[u8]) -> io::Result<()> {
        // The client encodes messages with serde_json, which escapes every
        // newline inside strings, so a message never splits a line.
        self.inner.write_all(message).await?;
        self.inner.write_all(b"\n").await?;
        self.inner.flush().await
    }

    async fn close(&mut self) -> io::Result<()> {
        self.inner.shutdown().await
    }
}

#[cfg(test)]
mod tests {
    use tokio::io::AsyncWriteExt;

    use super::*;

    fn limit(bytes: usize) -> NonZeroUsize {
        NonZeroUsize::new(bytes).unwrap()
    }

    async fn read_all(input: &[u8], max: usize) -> (Vec<Vec<u8>>, Option<ReceiveError>) {
        let mut reader = LineReader::new(std::io::Cursor::new(input.to_vec()), limit(max));
        let mut lines = Vec::new();
        loop {
            match reader.receive().await {
                Ok(Some(line)) => lines.push(line),
                Ok(None) => return (lines, None),
                Err(error) => return (lines, Some(error)),
            }
        }
    }

    #[tokio::test]
    async fn splits_lines_and_strips_carriage_returns() {
        let (lines, error) = read_all(b"a\r\n\nbc\n", 8).await;
        assert_eq!(lines, [b"a".to_vec(), Vec::new(), b"bc".to_vec()]);
        assert!(error.is_none());
    }

    #[tokio::test]
    async fn every_split_point_yields_the_same_lines() {
        let input = "{\"a\":\"é\"}\r\n{\"b\":\"😀\"}\n\n".as_bytes();
        for split in 0..=input.len() {
            let (client, mut server) = tokio::io::duplex(4);
            let mut reader = LineReader::new(client, limit(64));
            let (first, second) = input.split_at(split);
            let writer = async move {
                server.write_all(first).await.unwrap();
                tokio::task::yield_now().await;
                server.write_all(second).await.unwrap();
            };
            let read = async {
                let mut lines = Vec::new();
                for _ in 0..3 {
                    lines.push(reader.receive().await.unwrap().unwrap());
                }
                lines
            };
            let ((), lines) = tokio::join!(writer, read);
            assert_eq!(
                lines,
                [
                    "{\"a\":\"é\"}".as_bytes().to_vec(),
                    "{\"b\":\"😀\"}".as_bytes().to_vec(),
                    Vec::new()
                ],
                "split at {split}"
            );
            assert!(
                reader.receive().await.unwrap().is_none(),
                "split at {split}"
            );
        }
    }

    #[tokio::test]
    async fn a_line_at_the_limit_is_read_and_one_byte_more_is_refused() {
        let (lines, error) = read_all(b"12345\n", 5).await;
        assert_eq!(lines, [b"12345".to_vec()]);
        assert!(error.is_none());

        // Found with its newline in the same chunk.
        let (lines, error) = read_all(b"ok\n123456\nlater\n", 5).await;
        assert_eq!(lines, [b"ok".to_vec()]);
        assert!(matches!(error, Some(ReceiveError::TooLarge { limit: 5 })));

        // Refused before any newline arrives.
        let (lines, error) = read_all(&[b'x'; 64], 5).await;
        assert!(lines.is_empty());
        assert!(matches!(error, Some(ReceiveError::TooLarge { limit: 5 })));
    }

    #[tokio::test]
    async fn an_oversized_line_is_refused_without_reading_to_its_end() {
        let (client, mut server) = tokio::io::duplex(64);
        let mut reader = LineReader::new(client, limit(16));
        // The server never ends the line; the reader must not wait for it.
        // Bounded, so a reader that waits for the newline fails here instead
        // of hanging; `server` stays open until the test returns, so the
        // only way out is the limit check.
        server.write_all(&[b'x'; 17]).await.unwrap();
        let received = tokio::time::timeout(std::time::Duration::from_secs(10), reader.receive())
            .await
            .expect("the reader waited for a newline past the limit");
        assert!(matches!(
            received,
            Err(ReceiveError::TooLarge { limit: 16 })
        ));
        // Fused: the stream is not read further.
        assert!(matches!(
            reader.receive().await,
            Err(ReceiveError::TooLarge { .. })
        ));
        assert!(reader.buffer.capacity() == 0);
    }

    #[tokio::test]
    async fn a_stream_that_ends_inside_a_line_is_incomplete() {
        let (lines, error) = read_all(b"done\npartial", 64).await;
        assert_eq!(lines, [b"done".to_vec()]);
        assert!(matches!(error, Some(ReceiveError::Incomplete { bytes: 7 })));
    }

    #[tokio::test]
    async fn a_dropped_receive_loses_no_bytes() {
        let (client, mut server) = tokio::io::duplex(64);
        let mut reader = LineReader::new(client, limit(64));
        server.write_all(b"{\"par").await.unwrap();
        // Poll once, so the partial line is taken from the stream and the
        // read waits for more, then drop the future.
        tokio::select! {
            biased;
            _ = reader.receive() => panic!("no whole line was written"),
            () = std::future::ready(()) => {}
        }
        assert_eq!(reader.buffer, b"{\"par");
        server.write_all(b"tial\":1}\n").await.unwrap();
        assert_eq!(reader.receive().await.unwrap().unwrap(), b"{\"partial\":1}");
    }

    #[tokio::test]
    async fn the_writer_frames_each_message_and_closes_the_stream() {
        let (client, mut server) = tokio::io::duplex(64);
        let mut writer = LineWriter::new(client);
        writer.send(b"{}").await.unwrap();
        writer.send(b"[1]").await.unwrap();
        writer.close().await.unwrap();
        let mut written = Vec::new();
        server.read_to_end(&mut written).await.unwrap();
        assert_eq!(written, b"{}\n[1]\n");
    }
}
