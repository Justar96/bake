//! The runtime port: where the loop sends the view's runtime effects and
//! where runtime updates come from. Until a native runtime exists, the
//! only port is [`FixturePort`], which runs [`Fixture`] on its own thread.

use std::io;
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::thread::{self, JoinHandle};
use std::time::Instant;

use bake_tui_view::runtime::{RuntimeUpdate, Submission};

use crate::fixture::Fixture;

/// What the loop asks of the runtime.
#[derive(Debug)]
pub enum Request {
    Submit(Submission),
    Cancel,
    /// Send every waiting prompt now.
    SendPending,
}

/// Runs the fixture on a thread. Requests go in over a channel; updates
/// come out through `send`, on the loop's clock, in the order the fixture
/// produced them. Dropping or stopping the port joins the thread, so no
/// update arrives after it.
pub struct FixturePort {
    requests: Option<Sender<Request>>,
    thread: Option<JoinHandle<()>>,
}

impl FixturePort {
    /// Starts the thread. `send` returns `false` once the loop has gone, which
    /// ends the thread.
    pub fn start(
        clock: Instant,
        send: impl Fn(RuntimeUpdate) -> bool + Send + 'static,
    ) -> io::Result<Self> {
        let (requests, inbox) = mpsc::channel::<Request>();
        let thread = thread::Builder::new()
            .name("bake-tui-fixture".into())
            .spawn(move || {
                let mut fixture = Fixture::default();
                let emit = |updates: Vec<RuntimeUpdate>| updates.into_iter().all(&send);
                loop {
                    let request = match fixture.next_due() {
                        Some(due) => {
                            let wait = due.saturating_sub(clock.elapsed());
                            match inbox.recv_timeout(wait) {
                                Ok(request) => Some(request),
                                Err(RecvTimeoutError::Timeout) => None,
                                Err(RecvTimeoutError::Disconnected) => return,
                            }
                        }
                        None => match inbox.recv() {
                            Ok(request) => Some(request),
                            Err(_) => return,
                        },
                    };
                    let now = clock.elapsed();
                    // Updates already due go first, so a request lands
                    // where the timeline had reached.
                    let mut updates = fixture.advance(now);
                    match request {
                        Some(Request::Submit(submission)) => {
                            updates.extend(fixture.submit(submission, now));
                        }
                        Some(Request::Cancel) => updates.extend(fixture.cancel(now)),
                        Some(Request::SendPending) => updates.extend(fixture.send_pending(now)),
                        None => {}
                    }
                    updates.extend(fixture.advance(now));
                    if !emit(updates) {
                        return;
                    }
                }
            })?;
        Ok(Self {
            requests: Some(requests),
            thread: Some(thread),
        })
    }

    /// Passes a request on; one sent after the thread ended is dropped.
    pub fn request(&self, request: Request) {
        if let Some(requests) = &self.requests {
            let _ = requests.send(request);
        }
    }

    /// Ends the thread and waits for it; reports a panic in it.
    pub fn stop(mut self) -> io::Result<()> {
        self.join()
    }

    fn join(&mut self) -> io::Result<()> {
        // Closing the channel wakes the thread, which then returns.
        self.requests = None;
        match self.thread.take().map(JoinHandle::join) {
            Some(Err(_)) => Err(io::Error::other("the fixture runtime panicked")),
            _ => Ok(()),
        }
    }
}

impl Drop for FixturePort {
    fn drop(&mut self) {
        let _ = self.join();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bake_tui_view::state::Outcome;
    use std::time::Duration;

    /// How long a test waits for an update before failing.
    const PATIENCE: Duration = Duration::from_secs(10);

    #[test]
    fn a_submitted_prompt_runs_to_its_end_and_stop_joins_the_thread() {
        let (sender, updates) = mpsc::channel();
        let port = FixturePort::start(Instant::now(), move |u| sender.send(u).is_ok()).unwrap();
        port.request(Request::Submit(Submission::FollowUp("hi".into())));
        let mut seen = Vec::new();
        loop {
            let update = updates.recv_timeout(PATIENCE).expect("an update");
            let done = update == RuntimeUpdate::TurnEnded(Outcome::Completed);
            seen.push(update);
            if done {
                break;
            }
        }
        assert!(matches!(seen[1], RuntimeUpdate::TurnStarted { .. }));
        port.stop().unwrap();
        // The thread has ended, so its sender is gone.
        assert!(updates.recv().is_err());
    }

    #[test]
    fn a_cancel_ends_the_turn_at_once() {
        let (sender, updates) = mpsc::channel();
        let port = FixturePort::start(Instant::now(), move |u| sender.send(u).is_ok()).unwrap();
        port.request(Request::Submit(Submission::FollowUp("hi".into())));
        port.request(Request::Cancel);
        let ended = std::iter::from_fn(|| updates.recv_timeout(PATIENCE).ok())
            .find(|u| matches!(u, RuntimeUpdate::TurnEnded(_)));
        assert_eq!(ended, Some(RuntimeUpdate::TurnEnded(Outcome::Interrupted)));
        drop(port);
        assert!(updates.recv().is_err(), "dropping joins the thread");
    }
}
