//! The driver's requests from the view: prompts, steers, cancels, dialog
//! answers, and the stops of background work.

use super::*;

impl Driver {
    pub(super) async fn request(&mut self, request: Request, running: &mut Option<Running>) {
        match request {
            Request::Submit(prompt) => {
                let id = Some(prompt.id);
                let text = prompt.submission.text().to_owned();
                let templates = self.template_names();
                if let Some((name, argument)) = commands::parse(&text) {
                    if self.command.is_some() {
                        return self.refuse(
                            id,
                            text,
                            "Another command is running; your draft is kept.".into(),
                        );
                    }
                    if !prompt.images.is_empty() {
                        return self.refuse(
                            id,
                            text,
                            format!("/{name} takes no images; your draft is kept."),
                        );
                    }
                    if let Some(reason) = commands::refuse_argument(name, &argument) {
                        return self.refuse(id, text, reason);
                    }
                    if (commands::TRANSITIONS.contains(&name) || commands::IDLE.contains(&name))
                        && let Some(reason) = self.busy(running.as_ref())
                    {
                        return self.refuse(id, text, reason);
                    }
                    return self.command(id, text, name, argument);
                }
                if let Some(reason) = refused_command(&text, &templates) {
                    return self.refuse(id, text, reason);
                }
                // While a session is being replaced or compacted, nothing
                // reaches it.
                if let Some(command) = self.command.as_ref().filter(|command| command.transition) {
                    let reason = if command.name == commands::COMPACT {
                        "The session is being compacted; your draft is kept."
                    } else if command.name == "tree" {
                        bake_tui_view::runtime::TREE_BEING_NAVIGATED
                    } else {
                        "The session is being replaced; your draft is kept."
                    };
                    return self.refuse(id, text, reason.into());
                }
                // `/share` and `/bug` read the session they upload until
                // they end, so no turn starts under them.
                if let Some(command) = self
                    .command
                    .as_ref()
                    .filter(|command| commands::IDLE.contains(&command.name))
                {
                    let reason = copy::SESSION_BEING_READ.replace("{command}", command.name);
                    return self.refuse(id, text, reason);
                }
                let images: Vec<ImageContent> = prompt.images.iter().map(image_content).collect();
                match running {
                    // Pi's `onSubmit` while streaming: a steer. One with
                    // images waits for the next turn instead, so they stay
                    // with their prompt, and so does one sent after a
                    // cancel, which must not join the run it stops.
                    Some(run) if images.is_empty() && !run.cancelled => {
                        match self.session.steer(&text, Vec::new()).await {
                            Ok(()) => self.send(RuntimeUpdate::Admitted(prompt.id)),
                            Err(reason) => self.refuse(id, text, reason),
                        }
                    }
                    Some(_) => {
                        self.waiting.push_back(Queued { id, text, images });
                        self.publish_pending();
                    }
                    None => *running = Some(self.begin(Queued { id, text, images })),
                }
            }
            // Pi's `onEscape` while streaming.
            Request::Cancel => {
                // `/compact` runs as the session's work: Esc cancels it, as
                // Pi's `abortCompaction` does.
                if let Some(command) = self
                    .command
                    .as_ref()
                    .filter(|command| command.name == commands::COMPACT)
                {
                    command.abort.abort();
                    return;
                }
                // `/tree`'s branch summary runs as the session's work: Esc
                // cancels it, as Pi's `abortBranchSummary` does.
                if let Some(work) = self.command.as_ref().and_then(|command| {
                    command
                        .work
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .clone()
                }) {
                    work.abort();
                    return;
                }
                let Some(run) = running.as_mut().filter(|run| !run.cancelled) else {
                    return;
                };
                run.cancelled = true;
                let queued = self.session.clear_queue();
                let returned: Vec<String> = queued
                    .steering
                    .into_iter()
                    .chain(queued.follow_up)
                    .collect();
                if !returned.is_empty() {
                    self.send(RuntimeUpdate::Returned(returned));
                }
                for waiting in std::mem::take(&mut self.waiting) {
                    self.refuse(waiting.id, waiting.text, INTERRUPTED_PROMPT.into());
                }
                self.publish_pending();
                let session = Arc::clone(&self.session);
                self.aborts
                    .push(tokio::spawn(async move { session.abort().await }));
            }
            // Waiting prompts join the running turn at its next step; one
            // with images keeps waiting for its own turn.
            // The port answers approvals itself.
            Request::Answer { .. } => {}
            Request::StopJob(id) => {
                jobs::stop(self.session.jobs(), &id);
                self.publish_background();
            }
            Request::StopAgent(id) => {
                agents::stop(self.session.subagents(), &id);
                self.publish_background();
            }
            // To the running command, which drops an answer for a dialog
            // it no longer shows.
            Request::Dialog { id, answer } => {
                if let Some(command) = &self.command {
                    // Esc on the dialog shown now also aborts the work on
                    // its answer, such as a held check of the proxy; a
                    // commit already started runs on to its result.
                    let shown = command.shown.load(std::sync::atomic::Ordering::SeqCst);
                    if answer == DialogAnswer::Cancel && shown != 0 && id == shown {
                        command.abort.abort();
                    }
                    let _ = command.answers.send((id, answer));
                }
            }
            Request::SendPending => {
                if running.as_ref().is_none_or(|run| run.cancelled) {
                    return;
                }
                let mut kept = VecDeque::new();
                while let Some(queued) = self.waiting.pop_front() {
                    if !queued.images.is_empty() {
                        kept.push_back(queued);
                        continue;
                    }
                    match self.session.steer(&queued.text, Vec::new()).await {
                        Ok(()) => {
                            if let Some(id) = queued.id {
                                self.send(RuntimeUpdate::Admitted(id));
                            }
                        }
                        Err(reason) => self.refuse(queued.id, queued.text, reason),
                    }
                }
                self.waiting = kept;
                self.publish_pending();
            }
        }
    }
}
