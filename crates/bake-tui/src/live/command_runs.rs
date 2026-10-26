//! The driver's commands: starting one as a task it owns, refusing one
//! while the session is busy, applying how it ended, and swapping in the
//! session it built.

use super::*;

impl Driver {
    /// Starts the command `name` names as an owned task.
    pub(super) fn command(
        &mut self,
        id: Option<u64>,
        text: String,
        name: &'static str,
        argument: String,
    ) {
        let (answers, inbox) = mpsc::unbounded_channel();
        let abort = AbortController::new();
        let send = Arc::clone(&self.send);
        let shown = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let context = commands::Context::new(
            Arc::clone(&self.session),
            self.home.clone(),
            Arc::new(move |update| send(update)),
            inbox,
            abort.signal(),
            Arc::clone(&self.dialog_ids),
            Arc::clone(&shown),
        );
        let context = context.with_factory(self.factory.clone());
        let work = Arc::clone(&context.work);
        #[cfg(test)]
        let context = context.with_commit_gate(self.commit_gate.clone());
        #[cfg(test)]
        let context = context.with_gh(self.gh.clone());
        // The bar shows `/compact` as the running work, so Esc reaches it
        // as a cancel and Enter as a prompt the barrier refuses.
        if name == commands::COMPACT {
            self.projector
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .begin(None);
            self.turns += 1;
            self.send(RuntimeUpdate::TurnStarted {
                seed: format!("{}-{}", self.session.session_id(), self.turns),
            });
            self.send(RuntimeUpdate::CompactionStarted { interrupts: false });
        }
        let task = tokio::spawn(commands::run(name, argument, context));
        self.command = Some(CommandRun {
            task,
            shown,
            work,
            answers,
            abort,
            id,
            text,
            name,
            transition: commands::TRANSITIONS.contains(&name),
        });
    }

    /// Why the session cannot be replaced now, as the TypeScript
    /// navigation's `unavailable`: a turn runs, prompts wait, or the
    /// session compacts or retries. `None` when it can.
    pub(super) fn busy(&self, running: Option<&Running>) -> Option<String> {
        if running.is_some() || self.session.is_streaming() {
            return Some(
                "A turn is running; stop it with Esc or wait, and your draft is kept.".into(),
            );
        }
        if !self.waiting.is_empty() || !self.session.queued_messages().is_empty() {
            return Some("Prompts are waiting to be sent; your draft is kept.".into());
        }
        if self.session.is_compacting() || self.session.is_retrying() {
            return Some(
                "The session is busy; try again when it is idle. Your draft is kept.".into(),
            );
        }
        None
    }

    /// A command ended: its note goes to the transcript and its prompt is
    /// admitted, or its draft comes back with why. A replacement session is
    /// committed here ([`Self::replace`]).
    pub(super) async fn commanded(
        &mut self,
        done: Result<commands::Outcome, tokio::task::JoinError>,
        running: Option<&Running>,
    ) {
        let Some(command) = self.command.take() else {
            return;
        };
        let outcome = done.unwrap_or_else(|error| Err(format!("The command failed: {error}")));
        let compacted = matches!(outcome, Ok(commands::Done::Compacted { .. }));
        if command.name == commands::COMPACT && !compacted {
            // It opens no dialog, so no keyboard handoff starts and the
            // draft typed while it ran keeps taking keys. A compaction that
            // succeeded ends after the transcript is rebuilt, below, so its
            // outcome stays in the bar.
            self.send(RuntimeUpdate::TurnEnded(work_outcome(&outcome)));
        } else if !commands::NO_DIALOG.contains(&command.name) {
            // The command closed its dialog; a panicked one could not have.
            self.send(RuntimeUpdate::Dialog(None));
        }
        match outcome {
            Ok(commands::Done::Replace {
                replacement,
                note,
                draft,
            }) => {
                // Prompts were refused while it ran, so the session is
                // idle; anything else keeps it, and its draft comes back,
                // as does an Esc that arrived after the replacement was
                // built: the commit is here, and nothing has changed yet.
                let busy = if command.abort.signal().aborted() {
                    Some(commands::CANCELLED.to_owned())
                } else {
                    self.busy(running)
                };
                if let Some(reason) = busy {
                    replacement.session.shutdown().await;
                    self.refuse(command.id, command.text, reason);
                } else {
                    self.replace(*replacement, note).await;
                    if let Some(id) = command.id {
                        self.send(RuntimeUpdate::Admitted(id));
                    }
                    if let Some(draft) = draft {
                        self.send(RuntimeUpdate::Restored(draft));
                    }
                }
            }
            // The leaf already moved inside the session, which claimed it
            // idle; the transcript starts over with the new path.
            Ok(commands::Done::Navigated { note, draft }) => {
                if let Some(id) = command.id {
                    self.send(RuntimeUpdate::Admitted(id));
                }
                *self
                    .projector
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner) =
                    Projector::with_history(&self.session.messages());
                self.send(RuntimeUpdate::Replaced(opening_rows(
                    &[note],
                    &self.session,
                )));
                if let Some(draft) = draft {
                    self.send(RuntimeUpdate::Restored(draft));
                }
                self.publish_pending();
            }
            // The compaction is written and the agent's transcript is the
            // compacted context: the transcript starts over with it, as
            // Pi's `compaction_end` handler renders it.
            Ok(commands::Done::Compacted { row }) => {
                if let Some(id) = command.id {
                    self.send(RuntimeUpdate::Admitted(id));
                }
                *self
                    .projector
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner) =
                    Projector::with_history(&self.session.messages());
                let mut rows = projection::replay_compacted(&self.session.messages());
                rows.push(row);
                self.send(RuntimeUpdate::Replaced(rows));
                self.send(RuntimeUpdate::TurnEnded(Outcome::Completed));
                self.publish_pending();
            }
            Ok(done) => {
                if let Some(id) = command.id {
                    self.send(RuntimeUpdate::Admitted(id));
                }
                match done {
                    commands::Done::Note(note) => {
                        self.send(RuntimeUpdate::Commit(vec![Row::Info(note)]));
                    }
                    commands::Done::Rows(rows) => self.send(RuntimeUpdate::Commit(rows)),
                    _ => {}
                }
            }
            Err(reason) => self.refuse(command.id, command.text, reason),
        }
        self.status();
    }

    /// Replaces the idle session with `replacement`, as Pi's
    /// `AgentSessionRuntime` does after `teardownCurrent`: the startup
    /// refresh, which works for the old session, is aborted and awaited; the
    /// old session is shut down, awaiting its writes and releasing its file;
    /// then the new one takes its place, its events reach the loop through
    /// a fresh projector, and the transcript starts over with its replay.
    /// The old session's listeners are closed by its shutdown, so none of
    /// its events reach the new transcript, and its MCP servers have
    /// stopped before the new session starts its own
    /// ([`bake_coding_agent::cli::Replacement::start_servers`]).
    pub(super) async fn replace(
        &mut self,
        replacement: bake_coding_agent::cli::Replacement,
        note: String,
    ) {
        if let Some((task, abort)) = self.startup.take() {
            abort.abort();
            let _ = task.await;
        }
        for abort in self.aborts.drain(..) {
            let _ = abort.await;
        }
        let mut replacement = replacement;
        // The approval broker stays connected: the new session's tools ask
        // the same broker, which the old session's shutdown leaves open.
        // The old session's MCP servers are gone when this returns; only
        // then do the new session's start, as Pi tears the current session
        // down before it creates the next.
        self.session.shutdown().await;
        let started = replacement.start_servers();
        let bake_coding_agent::cli::Replacement {
            session,
            mut warnings,
            ..
        } = replacement;
        if let Err(message) = started {
            warnings.push(message);
        }
        let session = Arc::new(session);
        if let Some(subscription) = self.subscription.take() {
            subscription.unsubscribe();
        }
        *self
            .projector
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Projector::with_history(&session.messages());
        self.session = Arc::clone(&session);
        self.mcp = session.mcp_servers().map(McpNotices::new);
        *self.current.lock().unwrap_or_else(PoisonError::into_inner) = Arc::clone(&session);
        self.subscription = Some(subscribe(
            &session,
            &self.projector,
            &self.send,
            &self.status_due,
        ));
        self.turns = 0;
        let notes = replacement_notes(note, &session, warnings);
        self.send(RuntimeUpdate::Replaced(opening_rows(&notes, &session)));
        self.publish_background();
        self.publish_mcp();
        self.publish_pending();
    }
}
