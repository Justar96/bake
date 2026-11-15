//! User input on its way into a run: the steering and follow-up queues,
//! prompt images, and the text a prompt or a queued message becomes.

use super::*;

/// The session's queue of undelivered prompts and the order its updates
/// are announced in.
///
/// No lock is held while listeners run, so a listener may read or change
/// the queue. One caller at a time drains announcements: a change made
/// while another caller is announcing, from a listener or another thread,
/// only marks the queue changed, and the announcing caller announces the
/// latest queue again once its listeners return. Every listener therefore
/// sees the queue's states in order, ending with the latest, and never an
/// older queue after a newer one.
#[derive(Default)]
pub(super) struct Queue {
    messages: Mutex<QueuedMessages>,
    drain: Mutex<Drain>,
}

/// Whether a caller is announcing, and whether the queue changed since its
/// last snapshot.
#[derive(Default)]
struct Drain {
    announcing: bool,
    changed: bool,
}

impl Queue {
    pub(super) fn lock(&self) -> MutexGuard<'_, QueuedMessages> {
        self.messages.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn drain(&self) -> MutexGuard<'_, Drain> {
        self.drain.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Pi's `_emitQueueUpdate`: tells the listeners what waits now, or
    /// leaves that to the caller already announcing.
    pub(super) fn announce(&self, core: &Core) {
        {
            let mut drain = self.drain();
            if drain.announcing {
                drain.changed = true;
                return;
            }
            drain.announcing = true;
        }
        loop {
            self.drain().changed = false;
            let snapshot = self.lock().clone();
            core.emit(&SessionEvent::QueueUpdate {
                steering: &snapshot.steering,
                follow_up: &snapshot.follow_up,
            });
            let mut drain = self.drain();
            if !drain.changed {
                drain.announcing = false;
                return;
            }
        }
    }
}

/// The prompts [`AgentSession::steer`] and [`AgentSession::follow_up`]
/// queued that the agent has not delivered yet, Pi's `_steeringMessages`
/// and `_followUpMessages`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct QueuedMessages {
    /// Steering prompts, oldest first.
    pub steering: Vec<String>,
    /// Follow-up prompts, oldest first.
    pub follow_up: Vec<String>,
}

impl QueuedMessages {
    /// Whether nothing waits.
    pub fn is_empty(&self) -> bool {
        self.steering.is_empty() && self.follow_up.is_empty()
    }

    /// Pi's `message_start` bookkeeping: a delivered user prompt leaves the
    /// steering queue first, else the follow-up queue. Returns whether the
    /// queue changed.
    pub(super) fn delivered(&mut self, text: &str) -> bool {
        if let Some(index) = self.steering.iter().position(|queued| queued == text) {
            self.steering.remove(index);
            return true;
        }
        if let Some(index) = self.follow_up.iter().position(|queued| queued == text) {
            self.follow_up.remove(index);
            return true;
        }
        false
    }
}

/// Pi's `_normalizePromptImages`: decodes each prompt image and runs
/// [`process_image`] with `auto_resize_images` and the active model's
/// `inputLimits.images.resize`.
pub(super) fn normalize_prompt_images(
    images: &[ImageContent],
    auto_resize_images: bool,
    resize_options: ImageResizeOptions,
) -> (Vec<ImageContent>, Vec<String>) {
    use base64::Engine as _;
    let engine = base64::engine::GeneralPurpose::new(
        &base64::alphabet::STANDARD,
        base64::engine::GeneralPurposeConfig::new()
            .with_decode_padding_mode(base64::engine::DecodePaddingMode::Indifferent)
            .with_decode_allow_trailing_bits(true),
    );
    let mut normalized = Vec::with_capacity(images.len());
    let mut hints = Vec::new();
    for image in images {
        let cleaned: String = image
            .data
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '-' | '_'))
            .collect();
        let bytes = engine
            .decode(cleaned.replace('-', "+").replace('_', "/"))
            .unwrap_or_default();
        match process_image(&bytes, &image.mime_type, auto_resize_images, resize_options) {
            ProcessedImage::Failed(message) => hints.push(message),
            ProcessedImage::Ok {
                data,
                mime_type,
                hints: image_hints,
            } => {
                normalized.push(ImageContent { data, mime_type });
                hints.extend(image_hints);
            }
        }
    }
    (normalized, hints)
}

/// Applies `settings.json`'s `maxToolArgumentWhitespace` to every request
/// whose options leave it unset.
pub(super) fn with_tool_argument_whitespace(
    inner: bake_agent::StreamFn,
    max: Option<usize>,
) -> bake_agent::StreamFn {
    let Some(max) = max else {
        return inner;
    };
    Arc::new(move |model, context, mut options| {
        options.base.max_tool_argument_whitespace.get_or_insert(max);
        inner(model, context, options)
    })
}

/// Pi's `convertToLlmWithBlockImages` replacement text.
pub(super) const IMAGE_READING_DISABLED: &str = "Image reading is disabled.";

/// Pi's `blockImages` filter: images in user and tool-result messages
/// become one placeholder text per run of images.
pub(super) fn block_images(messages: Vec<Message>) -> Vec<Message> {
    fn filter(blocks: &[UserContentBlock]) -> Option<Vec<UserContentBlock>> {
        if !blocks
            .iter()
            .any(|block| matches!(block, UserContentBlock::Image(_)))
        {
            return None;
        }
        let mut filtered: Vec<UserContentBlock> = Vec::with_capacity(blocks.len());
        for block in blocks {
            let block = match block {
                UserContentBlock::Image(_) => {
                    UserContentBlock::Text(TextContent::new(IMAGE_READING_DISABLED))
                }
                other => other.clone(),
            };
            let repeated = matches!(
                (&block, filtered.last()),
                (UserContentBlock::Text(text), Some(UserContentBlock::Text(previous)))
                    if text.text == IMAGE_READING_DISABLED && previous.text == IMAGE_READING_DISABLED
            );
            if !repeated {
                filtered.push(block);
            }
        }
        Some(filtered)
    }
    messages
        .into_iter()
        .map(|message| match message {
            Message::User(mut user) => {
                if let UserContent::Blocks(blocks) = &user.content
                    && let Some(filtered) = filter(blocks)
                {
                    user.content = UserContent::Blocks(filtered);
                }
                Message::User(user)
            }
            Message::ToolResult(mut result) => {
                if let Some(filtered) = filter(&result.content) {
                    result.content = filtered;
                }
                Message::ToolResult(result)
            }
            other => other,
        })
        .collect()
}

impl AgentSession {
    /// Pi's `steer`: expands a `/skill:` command and prompt templates and
    /// normalizes images as [`Self::prompt`] does, then queues the prompt for
    /// the running turn's next step, after its tool calls and before its next
    /// model request. A prompt queued while the agent is idle is delivered
    /// with the next prompt's first turn. Refused once the session is shut
    /// down or a session write has failed.
    pub async fn steer(&self, text: &str, images: Vec<ImageContent>) -> Result<(), String> {
        self.queue_user_input(text, images, true).await
    }

    /// Pi's `followUp`: like [`Self::steer`], but delivered only when the
    /// agent would otherwise stop.
    pub async fn follow_up(&self, text: &str, images: Vec<ImageContent>) -> Result<(), String> {
        self.queue_user_input(text, images, false).await
    }

    /// The prompts queued and not delivered yet, Pi's
    /// `getSteeringMessages` and `getFollowUpMessages`.
    pub fn queued_messages(&self) -> QueuedMessages {
        self.queue.lock().clone()
    }

    /// Pi's `clearQueue`: empties both queues, the agent's included, and
    /// returns what they held, so an interface can give the text back.
    pub fn clear_queue(&self) -> QueuedMessages {
        let taken = {
            let mut queued = self.queue.lock();
            self.agent.clear_all_queues();
            std::mem::take(&mut *queued)
        };
        self.queue.announce(&self.core);
        taken
    }

    /// Why the session takes no more prompts: it is shut down, or a write
    /// to its file failed, so the file would miss a message.
    pub(super) fn closed_reason(&self) -> Option<String> {
        prompt_execution::closed_reason(&self.work, &self.listeners, &self.write_failure)
    }

    pub(crate) fn persistence_failure(&self) -> Option<String> {
        lock_gap(&self.write_failure).clone()
    }

    /// Pi's `_queueUserInput`, `_queueSteer`, and `_queueFollowUp`. The
    /// preparation's blocking work holds work permits, so shutdown waits
    /// for it; the session is checked again once it is done, and the prompt
    /// is queued under a permit, so admission cannot outlive shutdown.
    async fn queue_user_input(
        &self,
        text: &str,
        images: Vec<ImageContent>,
        steer: bool,
    ) -> Result<(), String> {
        if let Some(reason) = self.closed_reason() {
            return Err(reason);
        }
        let (text, images) = self.prepare_user_input(text, images, self.model()).await?;
        let context = match self.hooks.get() {
            Some(installed) => {
                hooks::before_queued_prompt(
                    Arc::clone(installed),
                    Arc::clone(&self.core),
                    Arc::clone(&self.persist_error),
                    self.agent.messages(),
                    text.clone(),
                )
                .await?
            }
            None => None,
        };
        let permit = self.work.enter();
        if let Some(reason) = self.closed_reason() {
            return Err(reason);
        }
        // The transcript a queued prompt would join is being replaced.
        if self.core.activity.borrow().navigation {
            return Err(NAVIGATION_WHILE_BUSY.to_owned());
        }
        let Some(_permit) = permit else {
            return Err("The session is shut down.".into());
        };
        let mut content = vec![UserContentBlock::Text(TextContent::new(text.clone()))];
        content.extend(images.into_iter().map(UserContentBlock::Image));
        let message = UserMessage {
            content: UserContent::Blocks(content),
            timestamp: bake_ai::now_ms(),
        };
        // The prompt and its hook context are one queued item, so they
        // reach the same step in either queue mode.
        let mut unit = vec![LiveMessage::from(message)];
        unit.extend(context);
        {
            let mut queued = self.queue.lock();
            if steer {
                queued.steering.push(text);
                self.agent.steer_all(unit);
            } else {
                queued.follow_up.push(text);
                self.agent.follow_up_all(unit);
            }
        }
        self.queue.announce(&self.core);
        Ok(())
    }

    /// Expands prompt commands and normalizes images under the session gate.
    pub(super) async fn prepare_user_input(
        &self,
        text: &str,
        images: Vec<ImageContent>,
        model: Option<Model>,
    ) -> Result<(String, Vec<ImageContent>), String> {
        prompt_execution::prepare_user_input(
            &self.work,
            &self.prompt_options,
            &self.prompt_templates,
            &self.settings,
            text,
            images,
            model,
        )
        .await
    }
}
