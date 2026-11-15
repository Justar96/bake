//! Piped standard input: read once, bounded, with a warning when nothing
//! arrives.

use super::*;

/// Pi's `readPipedStdin`: the trimmed input, or `None` when empty.
pub(super) fn read_piped_stdin(stdin: Box<dyn Read + Send>) -> Result<Option<String>, String> {
    let mut bytes = Vec::new();
    stdin
        .take(MAX_STDIN_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("Could not read standard input: {error}"))?;
    if bytes.len() as u64 > MAX_STDIN_BYTES {
        return Err(format!(
            "Standard input is larger than {MAX_STDIN_BYTES} bytes"
        ));
    }
    let text = String::from_utf8_lossy(&bytes);
    let trimmed = text.trim();
    Ok((!trimmed.is_empty()).then(|| trimmed.to_owned()))
}

/// What piped standard input gave the first prompt.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Piped {
    /// Read to its end: the trimmed text, or `None` when empty.
    Read(Option<String>),
    /// Nothing arrived within the wait, so it was left unread.
    Silent,
}

/// Reads `stdin` to its end ([`read_piped_stdin`]) on a thread of its own,
/// which a silent pipe may keep blocked until the process exits; nothing
/// waits for it then. With `first_byte_wait`, a stream that neither sends
/// a byte nor ends within it is [`Piped::Silent`]; once one arrives, it is
/// read to its end however long that takes.
pub(super) async fn read_stdin(
    stdin: Box<dyn Read + Send>,
    first_byte_wait: Option<std::time::Duration>,
) -> Result<Piped, String> {
    let (first, arrived) = tokio::sync::oneshot::channel();
    let (sent, done) = tokio::sync::oneshot::channel();
    let reader = FirstRead {
        inner: stdin,
        first: Some(first),
    };
    std::thread::Builder::new()
        .name("bake-stdin".into())
        .spawn(move || {
            let _ = sent.send(read_piped_stdin(Box::new(reader)));
        })
        .map_err(|error| format!("Could not read standard input: {error}"))?;
    if let Some(wait) = first_byte_wait {
        tokio::select! {
            biased;
            // A reader that stopped reports why below.
            _ = arrived => {}
            () = tokio::time::sleep(wait) => return Ok(Piped::Silent),
        }
    }
    done.await
        .map_err(|_| "Could not read standard input: its reader stopped".to_owned())?
        .map(Piped::Read)
}

/// Says when its first read returns, with bytes, the end, or an error.
struct FirstRead {
    inner: Box<dyn Read + Send>,
    first: Option<tokio::sync::oneshot::Sender<()>>,
}

impl Read for FirstRead {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let read = self.inner.read(buffer);
        let interrupted =
            matches!(&read, Err(error) if error.kind() == std::io::ErrorKind::Interrupted);
        if !interrupted && let Some(first) = self.first.take() {
            let _ = first.send(());
        }
        read
    }
}

/// The warning for piped input left unread after `wait`.
pub(super) fn silent_stdin_warning(wait: std::time::Duration) -> String {
    format!(
        "Nothing arrived on standard input within {} ms, so the prompt is sent without it. Pass - to wait for piped input, or redirect standard input from /dev/null to skip the wait.",
        wait.as_millis()
    )
}
