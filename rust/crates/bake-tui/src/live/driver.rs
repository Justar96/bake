//! The driver's loop and a prompt's life in it: what wakes it, how a
//! prompt is begun and settled, and how notices of background work open
//! a turn of their own.

use super::*;

impl Drop for Driver {
    /// A driver ends through its shutdown, which leaves nothing here; one
    /// that panicked aborts its command and startup refresh, as that
    /// shutdown does, and hands them to the port's handler to await, so a
    /// commit already started still finishes and no fetch outlives the
    /// port.
    fn drop(&mut self) {
        let mut orphans = self.orphans.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(command) = self.command.take() {
            command.abort.abort();
            let task = command.task;
            orphans.push(Box::pin(async move {
                // A replacement the command built is never used.
                if let Ok(Ok(done)) = task.await {
                    done.discard().await;
                }
            }));
        }
        if let Some((task, abort)) = self.startup.take() {
            abort.abort();
            orphans.push(Box::pin(async move {
                let _ = task.await;
            }));
        }
    }
}

impl Driver {
    pub(super) async fn run(mut self, mut inbox: mpsc::UnboundedReceiver<Request>) {
        let mut running: Option<Running> = None;
        self.status();
        self.publish_background();
        self.publish_mcp();
        // `--offline` stops the one startup network operation.
        if let Some(home) = self.home.clone().filter(|_| !self.offline) {
            let abort = AbortController::new();
            let session = Arc::clone(&self.session);
            let signal = abort.signal();
            let send = Arc::clone(&self.send);
            self.startup = Some((
                tokio::spawn(commands::startup_refresh(
                    session,
                    home,
                    signal,
                    Arc::new(move |update| send(update)),
                )),
                abort,
            ));
        }
        // `--resume` opens the session picker before the first prompt; the
        // prompts wait in `held` until it ends, so the replacement it
        // chooses is not refused for prompts that wait. Nobody typed it, so
        // it carries no text: a cancelled picker gives nothing back to the
        // composer ([`Self::refuse`]).
        if self.resume {
            self.resume = false;
            self.command(None, String::new(), commands::RESUME, String::new());
        } else {
            self.waiting.append(&mut self.held);
            if let Some(next) = self.waiting.pop_front() {
                running = Some(self.begin(next));
            }
        }
        self.publish_pending();
        let mut background = tokio::time::interval(BACKGROUND_EVERY);
        background.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        while !self.gone.is_set() {
            // Subscribed before the check, so a job that settles after it
            // still wakes the loop.
            let mut jobs = self.session.jobs().subscribe();
            if running.is_none() && self.command.is_none() && self.waiting.is_empty() {
                running = self.deliver_notices().await;
            }
            let idle = running.is_none() && self.command.is_none();
            let in_run = running.is_some();
            let next = {
                let settle = async {
                    match running.as_mut() {
                        Some(run) => (&mut run.task).await,
                        None => std::future::pending().await,
                    }
                };
                let commanded = async {
                    match self.command.as_mut() {
                        Some(command) => (&mut command.task).await,
                        None => std::future::pending().await,
                    }
                };
                let settled_job = async {
                    if idle && jobs.changed().await {
                        return;
                    }
                    std::future::pending::<()>().await;
                };
                let mcp_changed = async {
                    match &mut self.mcp {
                        Some(mcp) => mcp.changed().await,
                        None => std::future::pending().await,
                    }
                };
                tokio::select! {
                    joined = settle => Next::Settled(joined),
                    () = settled_job => Next::Jobs,
                    done = commanded => Next::Commanded(done),
                    () = mcp_changed => Next::McpChanged,
                    _ = background.tick() => Next::Background,
                    // Between prompts, turns and commands send the status
                    // as they end; a wake left over is taken by the next run.
                    () = self.status_due.notified(), if in_run => Next::Status,
                    request = inbox.recv() => Next::Request(request),
                    () = self.gone.notify.notified() => Next::Request(None),
                }
            };
            match next {
                Next::Settled(joined) => {
                    if let Some(run) = running.take() {
                        self.settled(run, joined).await;
                    }
                    if let Some(next) = self.waiting.pop_front() {
                        running = Some(self.begin(next));
                        self.publish_pending();
                    }
                }
                // Checked at the top of the loop.
                Next::Jobs => {}
                Next::Commanded(done) => {
                    self.commanded(done, running.as_ref()).await;
                    // The startup picker ended: the command line's prompts
                    // become ordinary waiting prompts, in order.
                    if !self.held.is_empty() && self.command.is_none() {
                        self.waiting.append(&mut self.held);
                        if running.is_none()
                            && let Some(next) = self.waiting.pop_front()
                        {
                            running = Some(self.begin(next));
                        }
                        self.publish_pending();
                    }
                }
                Next::McpChanged => self.publish_mcp(),
                Next::Background => self.publish_background(),
                Next::Status => self.refresh_status(),
                Next::Request(None) => break,
                Next::Request(Some(request)) => self.request(request, &mut running).await,
            }
        }
        // A command's dialog reads as cancelled and its fetch is aborted;
        // a commit it has started runs on, and is awaited below.
        let command = self.command.take().map(|command| {
            command.abort.abort();
            drop(command.answers);
            command.task
        });
        let startup = self.startup.take().map(|(task, abort)| {
            abort.abort();
            task
        });
        // Pi's `dispose`, awaited: the run, its writes, and its requests end
        // before the terminal is given back.
        shut_down(&self.session, self.approvals.as_ref()).await;
        if let Some(subscription) = self.subscription.take() {
            subscription.unsubscribe();
        }
        if let Some(command) = command
            && let Ok(Ok(done)) = command.await
        {
            // A replacement the command built is never used.
            done.discard().await;
        }
        if let Some(startup) = startup {
            let _ = startup.await;
        }
        if let Some(run) = running {
            let _ = run.task.await;
        }
        for abort in self.aborts.drain(..) {
            let _ = abort.await;
        }
    }

    pub(super) fn send(&self, update: RuntimeUpdate) {
        (self.send)(update);
    }

    pub(super) fn refuse(&self, id: Option<u64>, text: String, reason: String) {
        match id {
            Some(id) => {
                self.send(RuntimeUpdate::Refused { id, reason });
            }
            // A command-line prompt has no draft; its text comes back. The
            // startup picker has no text, so nothing comes back and no
            // notice shows.
            None if !text.trim().is_empty() => {
                self.send(RuntimeUpdate::Returned(vec![text]));
            }
            None => {}
        }
    }

    pub(super) fn begin(&mut self, prompt: Queued) -> Running {
        self.projector
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .begin(prompt.id);
        self.turns += 1;
        self.send(RuntimeUpdate::TurnStarted {
            seed: format!("{}-{}", self.session.session_id(), self.turns),
        });
        let session = Arc::clone(&self.session);
        let Queued { id, text, images } = prompt;
        let sent = text.clone();
        let task = tokio::spawn(async move {
            session
                .prompt_with_control(&sent, images, RunControl::default())
                .await
        });
        Running {
            task,
            id,
            text,
            cancelled: false,
            notice: false,
        }
    }

    /// Opens a turn for the notices of background jobs that settled, and
    /// the messages and notices of subagents that arrived, while the
    /// session was idle, `tool-jobs`' `wakeup` delivery, while the
    /// wake budget lasts; past it the session keeps them for the next
    /// prompt ([`AgentSession::deliver_job_notices`]).
    pub(super) async fn deliver_notices(&mut self) -> Option<Running> {
        let jobs = self.session.jobs();
        // A settled job not yet reported, or a subagent's message or
        // settlement notice.
        if !jobs.has_undelivered() {
            return None;
        }
        if !jobs.notice_would_wake() {
            // Kept for the next prompt; no turn opens.
            let _ = self.session.deliver_job_notices().await;
            return None;
        }
        self.projector
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .begin(None);
        self.turns += 1;
        self.send(RuntimeUpdate::TurnStarted {
            seed: format!("{}-{}", self.session.session_id(), self.turns),
        });
        let session = Arc::clone(&self.session);
        let task = tokio::spawn(async move {
            session
                .deliver_job_notices_with_control(RunControl::default())
                .await
        });
        Some(Running {
            task,
            id: None,
            text: String::new(),
            cancelled: false,
            notice: true,
        })
    }

    pub(super) async fn settled(
        &mut self,
        run: Running,
        joined: Result<Result<RunOutcome, String>, tokio::task::JoinError>,
    ) {
        // The run's abort ends before anything else starts.
        for abort in self.aborts.drain(..) {
            let _ = abort.await;
        }
        let (admitted, displayed_error, mut updates) = {
            let mut projector = self
                .projector
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            (
                projector.admitted(),
                projector.last_error().map(str::to_owned),
                projector.finish(),
            )
        };
        let (outcome, failure) = match joined {
            Ok(Ok(RunOutcome::FinishedTurn)) => (Outcome::Completed, None),
            Ok(Ok(RunOutcome::Cancelled)) => (Outcome::Interrupted, None),
            Ok(Ok(outcome @ RunOutcome::BudgetExhausted { .. })) => {
                (Outcome::Failed, Some(outcome.to_string()))
            }
            Ok(Ok(RunOutcome::Failed { message })) | Ok(Err(message)) => {
                (Outcome::Failed, Some(message))
            }
            Err(error) => (
                Outcome::Failed,
                Some(format!("The agent task failed: {error}")),
            ),
        };
        for update in updates.drain(..) {
            self.send(update);
        }
        if admitted {
            if let Some(message) =
                failure.filter(|message| Some(message) != displayed_error.as_ref())
            {
                self.send(RuntimeUpdate::Commit(vec![Row::Error(message)]));
            }
        } else if run.notice {
            // A notice turn has no draft to give back.
            if let Some(message) = failure {
                self.send(RuntimeUpdate::Commit(vec![Row::Error(message)]));
            }
        } else {
            // The agent never took the prompt: refused, such as without an
            // API key, or cancelled first. Its draft goes back.
            let reason = failure.unwrap_or_else(|| INTERRUPTED_PROMPT.into());
            self.refuse(run.id, run.text, reason);
        }
        self.send(RuntimeUpdate::TurnEnded(outcome));
        self.status();
        if run.cancelled {
            return;
        }
        // A steer that arrived as the run was ending stays queued with no
        // run to take it; it runs next, ahead of the other waiting prompts.
        if !self.session.queued_messages().is_empty() {
            let queued = self.session.clear_queue();
            for text in queued.steering.into_iter().chain(queued.follow_up).rev() {
                self.waiting.push_front(Queued {
                    id: None,
                    text,
                    images: Vec::new(),
                });
            }
            self.publish_pending();
        }
    }
}

/// How a command that ran as the session's work ended, for the bar. Only
/// the command's own cancellation is an interruption: a failure, such as a
/// compaction whose write failed before a late Esc, stays a failure.
pub(super) fn work_outcome(outcome: &commands::Outcome) -> Outcome {
    match outcome {
        Ok(_) => Outcome::Completed,
        Err(reason) if reason == commands::CANCELLED => Outcome::Interrupted,
        Err(_) => Outcome::Failed,
    }
}

/// Why a prompt a cancel stopped before the agent took it came back.
pub(super) const INTERRUPTED_PROMPT: &str =
    "Stopped before it was sent; your prompt is back in the draft.";

/// Why a slash prompt is refused: it names one of [`DEFERRED_COMMANDS`],
/// or another command-like word that is neither `/skill:name` nor one of
/// `templates`. Text whose first word is
/// not command-like, such as a path, goes to the model as Pi sends it.
pub(super) fn refused_command(text: &str, templates: &[String]) -> Option<String> {
    let word = text
        .trim_start()
        .strip_prefix('/')?
        .split_whitespace()
        .next()?;
    if word.starts_with("skill:") || templates.iter().any(|name| name == word) {
        return None;
    }
    let command_like = word
        .chars()
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic())
        && word
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if DEFERRED_COMMANDS.contains(&word) {
        Some(deferred_command(word))
    } else if command_like {
        Some(format!(
            "/{word} is not a command or prompt template; your draft is kept."
        ))
    } else {
        None
    }
}

/// The refusal of a command in [`DEFERRED_COMMANDS`].
pub(super) fn deferred_command(word: &str) -> String {
    format!(
        "/{word} is not available in bake yet; it is planned for a later 0.4.x release, and your draft is kept."
    )
}
