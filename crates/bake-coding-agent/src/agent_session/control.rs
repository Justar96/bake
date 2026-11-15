//! Changes to a running session's settings, model, thinking level, and
//! queue modes, each run as one owned change ([`Control`]).

use super::*;

/// What one change ([`AgentSession::run_change`]) works with: the parts
/// of the session it reads and changes, owned by its task.
pub(super) struct Control {
    pub(super) agent: Arc<Agent>,
    pub(super) core: Arc<Core>,
    pub(super) session: SharedSession,
    registry: ModelRuntime,
    pub(super) settings: SharedSettings,
    settings_store: Option<SettingsStore>,
    selection: SessionSelection,
    pub(super) listeners: Arc<Listeners>,
    persist_error: Arc<Mutex<Option<String>>>,
    /// A share of the change's admission: its blocking work runs under it,
    /// so shutdown, which never refuses an admitted claim, waits for it.
    permit: WorkPermit,
}

impl Control {
    /// Runs `work` on the blocking pool under the change's permit.
    pub(super) async fn blocking<R: Send + 'static>(
        &self,
        work: impl FnOnce() -> R + Send + 'static,
        what: &str,
    ) -> Result<R, String> {
        self.permit
            .spawn_blocking(work)
            .map_err(|refused| format!("{what} task failed: {refused}"))?
            .await
            .map_err(|error| format!("{what} task failed: {error}"))
    }

    /// Appends to the session file on the blocking pool, under the
    /// change's permit, so shutdown waits for it.
    pub(super) async fn append(
        &self,
        write: impl FnOnce(&mut SessionManager) -> Result<(), crate::session::SessionError>
        + Send
        + 'static,
    ) -> Result<(), String> {
        self.write(write).await.map_err(|refused| refused.message())
    }

    /// [`Self::append`], keeping the session's error.
    async fn write(
        &self,
        write: impl FnOnce(&mut SessionManager) -> Result<(), crate::session::SessionError>
        + Send
        + 'static,
    ) -> Result<(), WriteRefused<crate::session::SessionError>> {
        let session = Arc::clone(&self.session);
        let core = Arc::clone(&self.core);
        let agent = Arc::clone(&self.agent);
        let written = self
            .permit
            .spawn_blocking(move || {
                let written = write_session(&session, &core.gap, write);
                // The run stops here, as soon as the write has failed and the
                // session lock is released, so a turn between steps takes no
                // further step while this change's task waits for a worker.
                if written.is_err() {
                    core.abort_requested.store(true, Ordering::SeqCst);
                    agent.abort();
                }
                written
            })
            // An admitted change's blocking work is never refused for a
            // closing gate; only a missing runtime refuses it, reported as
            // a write that never started.
            .map_err(|refused| {
                WriteRefused::AfterGap(format!("Session I/O task failed: {refused}"))
            })?
            .await
            // A task that did not finish reports only its reason, as a write
            // refused before it started does.
            .map_err(|error| WriteRefused::AfterGap(format!("Session I/O task failed: {error}")))?;
        // Off the async workers, with the hooks' lock released, so a test
        // that holds the change here never holds up the run it watches.
        #[cfg(test)]
        {
            let before = self.core.hooks().before_continuing.take();
            if let Some(before) = before {
                tokio::task::spawn_blocking(before)
                    .await
                    .expect("the test hook");
            }
        }
        written
    }

    /// A change's session write failed: the file now lacks an entry, so the
    /// running turn stops before its next request, no later message is
    /// written past the gap, and no later prompt is accepted. Returns the
    /// reason, naming `what` failed.
    pub(super) fn stop_after_failed_write(&self, error: &str, what: &str) -> String {
        // The gap is recorded, and the run stopped, at the write boundary
        // ([`Self::append`]); this reports the failure to the run's prompt.
        self.persist_error
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get_or_insert_with(|| error.to_owned());
        format!(
            "{what} could not be written to the session file ({error}), so the session stopped and takes no more prompts."
        )
    }

    /// Applies a model and thinking level to the agent and the published
    /// selection together, so the next request reads one change.
    fn apply(&self, model: Option<Model>, level: ModelThinkingLevel) {
        let mut published = self.selection.lock();
        match &model {
            Some(model) => {
                self.agent
                    .set_model_and_thinking_level(model.clone(), level);
                self.core.has_model.store(true, Ordering::SeqCst);
            }
            None => self.agent.set_thinking_level(level),
        }
        *published = Selected {
            model: model.or_else(|| self.core.model()),
            thinking_level: level,
        };
    }

    /// Saves a setting on the blocking pool.
    pub(super) async fn save(
        &self,
        save: impl FnOnce(&SettingsStore) -> Result<Settings, crate::json_store::StoreError>
        + Send
        + 'static,
    ) -> DefaultSave {
        let Some(store) = self.settings_store.clone() else {
            return DefaultSave::Failed("no settings file to save to".to_owned());
        };
        match self.blocking(move || save(&store), "Settings").await {
            Ok(Ok(_)) => DefaultSave::Saved,
            Ok(Err(error)) if error.is_committed() => DefaultSave::Unsynced(error.to_string()),
            Ok(Err(error)) => DefaultSave::Failed(error.to_string()),
            Err(error) => DefaultSave::Failed(error),
        }
    }
}

impl AgentSession {
    /// Sets `statusLine.tokens`: whether the interactive status line shows
    /// the session's input and output token totals. Saved; the terminal
    /// reads it when it next draws the status.
    pub async fn set_status_line_tokens(&self, on: bool) -> Result<DefaultSave, String> {
        self.set_status_line("tokens", on).await
    }

    /// Sets `statusLine.cache`: whether the interactive status line shows
    /// the session's prompt-cache reads, writes, and latest hit share.
    pub async fn set_status_line_cache(&self, on: bool) -> Result<DefaultSave, String> {
        self.set_status_line("cache", on).await
    }

    async fn set_status_line(&self, key: &'static str, on: bool) -> Result<DefaultSave, String> {
        self.run_change(move |control| async move {
            {
                let mut settings = write_settings(&control.settings);
                match key {
                    "tokens" => settings.status_line.tokens = on,
                    _ => settings.status_line.cache = on,
                }
            }
            Ok(control
                .save(move |store| store.set_status_line(key, on))
                .await)
        })
        .await
    }

    /// A work permit for a change, unless the session takes no more: a
    /// change after shutdown is refused rather than kept in memory.
    pub(super) fn change_permit(&self) -> Result<WorkPermit, String> {
        if let Some(reason) = self.closed_reason() {
            return Err(reason);
        }
        self.work
            .enter()
            .ok_or_else(|| "The session is shut down.".to_owned())
    }

    /// Runs one change as owned work: it waits its turn behind the other
    /// changes, is admitted once with a work permit (refused after
    /// shutdown), and then runs to completion on its own task, which holds
    /// the turn and the permit. A caller that stops waiting therefore
    /// neither abandons a half-written change nor lets the next change
    /// start before it ends, and shutdown waits for it.
    pub(super) async fn run_change<R, F>(
        &self,
        change: impl FnOnce(Control) -> F,
    ) -> Result<R, String>
    where
        R: Send + 'static,
        F: std::future::Future<Output = Result<R, String>> + Send + 'static,
    {
        let turn = Arc::clone(&self.control).lock_owned().await;
        let permit = self.change_permit()?;
        let work = change(Control {
            permit: permit.share(),
            agent: Arc::clone(&self.agent),
            core: Arc::clone(&self.core),
            session: Arc::clone(&self.session),
            registry: self.registry.clone(),
            settings: Arc::clone(&self.settings),
            settings_store: self.settings_store.clone(),
            selection: self.selection.clone(),
            listeners: Arc::clone(&self.listeners),
            persist_error: Arc::clone(&self.persist_error),
        });
        // A change ignores its task's cancellation: it is short, and a
        // half-applied change would leave the file and memory apart.
        permit
            .spawn(move |_scope| async move {
                let _turn = turn;
                work.await
            })
            .map_err(refused_message)?
            .await
            .map_err(|error| format!("The change failed: {error}"))?
    }

    /// Pi's `setThinkingLevel`: clamps to the model, records a change in the
    /// session file, then applies and announces it, and saves `level` as
    /// the default when `persist` is set.
    ///
    /// Serialized with the other changes and owned as `Self::run_change`
    /// describes; refused once the session is shut down. A change takes
    /// effect at the next model request, including the next request of a
    /// run that is streaming. When the session write fails, the change is
    /// not applied, the running turn stops, nothing more is written, and
    /// later prompts are refused, since the file would otherwise miss an
    /// entry. A failed save of the default leaves the session changed and
    /// is reported as [`DefaultSave::Failed`].
    pub async fn set_thinking_level(
        &self,
        level: ModelThinkingLevel,
        persist: bool,
    ) -> Result<DefaultSave, String> {
        self.run_change(move |control| async move {
            let effective = match control.core.model() {
                Some(model) => bake_ai::models::clamp_thinking_level(&model, level),
                None => level,
            };
            if effective != control.agent.thinking_level() {
                let written = control
                    .append(move |session| {
                        session
                            .append_thinking_level_change(effective.as_str())
                            .map(|_| ())
                    })
                    .await;
                if let Err(error) = written {
                    return Err(control.stop_after_failed_write(&error, "The thinking level"));
                }
                control.apply(None, effective);
                control
                    .listeners
                    .emit(&SessionEvent::ThinkingLevelChanged(effective));
            }
            if !persist {
                return Ok(DefaultSave::NotRequested);
            }
            let saved = control
                .save(move |store| store.set_default_thinking_level(level))
                .await;
            if saved.is_written() {
                write_settings(&control.settings).default_thinking_level = Some(level);
            }
            Ok(saved)
        })
        .await
    }

    /// Pi's `setModel`: refuses a model without auth, records the change and
    /// the thinking level the model's settings choose (a per-model level,
    /// else the default level, else the current one), applies both
    /// together, and saves the model as the default when `persist` is set.
    /// Serialized, owned, refused after shutdown, and effective from the
    /// next model request, as [`Self::set_thinking_level`] is. The model
    /// and thinking-level entries are one group
    /// ([`SessionManager::append_group`]), written in order with no entry
    /// between them, and the change is applied only once both are written.
    /// When the group's write fails, the session applies the entries known
    /// to be synced, then stops as for any failed write: nothing when the
    /// model entry is not known to be synced, else the new model with the
    /// previous level clamped to it. A line written without a sync may
    /// still be read by a later open of the file, so that open can differ
    /// ([`crate::session::GroupWriteError`]).
    pub async fn set_model(&self, model: Model, persist: bool) -> Result<DefaultSave, String> {
        self.run_change(move |control| async move {
            if control
                .registry
                .snapshot()
                .check_auth(&model.provider)
                .is_none()
            {
                return Err(format!("No API key for {}/{}", model.provider, model.id));
            }
            let previous = control.agent.thinking_level();
            let level = {
                let settings = read_settings(&control.settings);
                settings
                    .thinking_level_for(&model)
                    .or(settings.default_thinking_level)
                    .unwrap_or(previous)
            };
            let effective = bake_ai::models::clamp_thinking_level(&model, level);
            let changes_level = effective != previous;
            // One group, written under one hold of the session: no message
            // lands between the model and its level.
            let mut group = vec![GroupEntry::model_change(&model.provider, &model.id)];
            if changes_level {
                group.push(GroupEntry::thinking_level_change(effective.as_str()));
            }
            #[cfg(test)]
            let injected = control.core.hooks().fail_model_entry.take();
            #[cfg(test)]
            let core = Arc::clone(&control.core);
            let written = control
                .write(move |session| {
                    #[cfg(test)]
                    if let Some(hook) = injected {
                        core.hooks().before_continuing = Some(hook());
                        return Err(injected_failure());
                    }
                    session.append_group(group).map(|_| ())
                })
                .await;
            match written {
                Ok(()) => {}
                // The model change is synced and its thinking level is not
                // known to be: the session applies the known synced prefix,
                // the new model with the previous level clamped to it, and
                // halts. A reopen projects the same only when the level's
                // line is not readable; an unsynced line, or a torn one
                // whose whole JSON was written, reads as the new level.
                Err(WriteRefused::Failed(SessionError::GroupWrite(failure)))
                    if !failure.written.is_empty() =>
                {
                    let kept = bake_ai::models::clamp_thinking_level(&model, previous);
                    control.apply(Some(model.clone()), kept);
                    return Err(control.stop_after_failed_write(
                        &failure.to_string(),
                        "The model change was recorded, but its thinking level",
                    ));
                }
                Err(refused) => {
                    return Err(
                        control.stop_after_failed_write(&refused.message(), "The model change")
                    );
                }
            }
            control.apply(Some(model.clone()), effective);
            if changes_level {
                control
                    .listeners
                    .emit(&SessionEvent::ThinkingLevelChanged(effective));
            }
            if !persist {
                return Ok(DefaultSave::NotRequested);
            }
            let (provider, id) = (model.provider.clone(), model.id.clone());
            let saved = control
                .save(move |store| store.set_default_model_and_provider(&provider, &id))
                .await;
            if saved.is_written() {
                let mut settings = write_settings(&control.settings);
                settings.default_provider = Some(model.provider);
                settings.default_model = Some(model.id);
            }
            Ok(saved)
        })
        .await
    }

    /// Pi's `setSteeringMode`: applies the mode to the agent's steering
    /// queue now and saves it. Serialized and owned as the other changes.
    pub async fn set_steering_mode(&self, mode: QueueMode) -> Result<DefaultSave, String> {
        self.run_change(move |control| async move {
            control.agent.set_steering_mode(mode);
            write_settings(&control.settings).steering_mode = mode;
            Ok(control
                .save(move |store| store.set_steering_mode(mode))
                .await)
        })
        .await
    }

    /// Pi's `setFollowUpMode`: as [`Self::set_steering_mode`], for the
    /// follow-up queue.
    pub async fn set_follow_up_mode(&self, mode: QueueMode) -> Result<DefaultSave, String> {
        self.run_change(move |control| async move {
            control.agent.set_follow_up_mode(mode);
            write_settings(&control.settings).follow_up_mode = mode;
            Ok(control
                .save(move |store| store.set_follow_up_mode(mode))
                .await)
        })
        .await
    }

    /// Pi's `setAutoCompactionEnabled`: threshold and overflow compaction
    /// follow `enabled` from the next check, and it is saved as
    /// `compaction.enabled`.
    pub async fn set_auto_compaction_enabled(&self, enabled: bool) -> Result<DefaultSave, String> {
        self.run_change(move |control| async move {
            crate::settings::set_compaction_enabled_in(
                &mut write_settings(&control.settings),
                enabled,
            );
            Ok(control
                .save(move |store| store.set_compaction_enabled(enabled))
                .await)
        })
        .await
    }

    /// Sets Bake's `compaction.thresholdPercent`: automatic compaction also
    /// starts once the context fills `percent` of the window, from 1 to
    /// 100, from the next check ([`crate::compaction::CompactionSettings`]).
    /// `None` removes it, keeping Pi's rule alone.
    pub async fn set_compaction_threshold_percent(
        &self,
        percent: Option<u8>,
    ) -> Result<DefaultSave, String> {
        if percent.is_some_and(|percent| !(1..=100).contains(&percent)) {
            return Err("The compaction threshold is a whole percentage from 1 to 100.".into());
        }
        self.set_compaction_member("thresholdPercent", percent.map(serde_json::Value::from))
            .await
    }

    /// Sets Pi's `compaction.keepRecentTokens`, the recent tokens a
    /// compaction keeps, from the next compaction; `None` removes it, so
    /// Pi's default applies. A model's `modelOverrides` entry still wins.
    pub async fn set_compaction_keep_recent_tokens(
        &self,
        tokens: Option<u64>,
    ) -> Result<DefaultSave, String> {
        self.set_compaction_member("keepRecentTokens", tokens.map(serde_json::Value::from))
            .await
    }

    /// Sets Bake's `compaction.cachedSummary`: whether a compaction writes
    /// its summary from the provider's prompt cache
    /// ([`crate::compaction::cached`]), from the next request on.
    pub async fn set_cached_compaction(&self, enabled: bool) -> Result<DefaultSave, String> {
        self.set_compaction_member("cachedSummary", Some(serde_json::Value::Bool(enabled)))
            .await
    }

    async fn set_compaction_member(
        &self,
        key: &'static str,
        value: Option<serde_json::Value>,
    ) -> Result<DefaultSave, String> {
        self.run_change(move |control| async move {
            crate::settings::set_compaction_member_in(
                &mut write_settings(&control.settings),
                key,
                value.clone(),
            );
            Ok(control
                .save(move |store| store.set_compaction_member(key, value))
                .await)
        })
        .await
    }

    /// Sets `telecom`. A session joins its repository's team when it is
    /// built, so the change applies from the next session (`/new`,
    /// `/resume`, or a restart); this one keeps the team state it started
    /// with.
    pub async fn set_telecom_setting(&self, on: bool) -> Result<DefaultSave, String> {
        self.run_change(move |control| async move {
            write_settings(&control.settings).telecom = on;
            Ok(control.save(move |store| store.set_telecom(on)).await)
        })
        .await
    }

    /// Sets `visualize`. A session's tools are chosen when it is built, so
    /// the change applies from the next session (`/new`, `/resume`, or a
    /// restart); this one keeps the tools it started with.
    pub async fn set_visualize_setting(&self, on: bool) -> Result<DefaultSave, String> {
        self.run_change(move |control| async move {
            write_settings(&control.settings).visualize = on;
            Ok(control.save(move |store| store.set_visualize(on)).await)
        })
        .await
    }

    /// Pi v1.5.2's `setMermaidRenderingMode`: sets `markdown.mermaid`, how
    /// the interactive terminal draws Mermaid fences in answers. Saved; the
    /// terminal draws with it once it is told.
    pub async fn set_mermaid_rendering(
        &self,
        mode: crate::settings::MermaidRendering,
    ) -> Result<DefaultSave, String> {
        self.run_change(move |control| async move {
            write_settings(&control.settings).mermaid = mode;
            Ok(control.save(move |store| store.set_mermaid(mode)).await)
        })
        .await
    }

    /// Sets `branchSummary.skipPrompt`: whether `/tree` leaves a branch
    /// without asking to summarize it, from the next navigation.
    pub async fn set_branch_summary_skip_prompt(&self, skip: bool) -> Result<DefaultSave, String> {
        self.run_change(move |control| async move {
            write_settings(&control.settings).branch_summary_skip_prompt = skip;
            Ok(control
                .save(move |store| store.set_branch_summary_skip_prompt(skip))
                .await)
        })
        .await
    }

    /// Pi's `steeringMode`.
    pub fn steering_mode(&self) -> QueueMode {
        self.agent.steering_mode()
    }

    /// Pi's `followUpMode`.
    pub fn follow_up_mode(&self) -> QueueMode {
        self.agent.follow_up_mode()
    }
}
