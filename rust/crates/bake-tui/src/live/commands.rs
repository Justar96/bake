//! The commands a live session runs: `/model`, `/thinking`, `/settings`,
//! `/login`, `/logout`, and `/refresh-models`.
//!
//! `/model`, `/thinking`, and `/settings` follow Pi v1.2.0
//! `packages/coding-agent/src/modes/interactive/interactive-mode.ts`
//! (`handleModelCommand`, `showModelSelector`, the thinking selector, and
//! `showSettingsSelector`). A model or level named exactly is set at once
//! (Pi regression 7443). Otherwise a list opens. Enter changes this
//! session; Ctrl+S also saves the default, as Pi's `app.models.save` and
//! `app.thinking.save` do. `/settings` saves what it changes, as Pi's
//! settings selector does, and offers only settings the running session
//! applies at once.
//!
//! `/login`, `/logout`, and `/refresh-models` keep Bake's CLIProxyAPI
//! contract: `apps/tui/packages/app/src/cliproxyapi.ts`
//! (`configureCliProxyApi`, `refreshCliProxyModels`) and
//! `apps/tui/packages/app/src/login.ts` (`logout`). They write through
//! [`bake_coding_agent::cliproxyapi::account`].
//!
//! Each command runs as one task the driver owns. The task asks through
//! dialogs and takes their answers by id, so a stale answer is dropped.
//! Up to the commit, a cancel or shutdown ends the task with nothing
//! written: the fetch observes the abort signal, and a dialog whose answers
//! stop coming reads as cancelled. The commit and the runtime's update then
//! run on the blocking pool inside the task, which no abort stops. On
//! shutdown the driver aborts the task, shuts the session down, and then
//! awaits the task, so a commit it started has finished before the
//! terminal is given back. A driver that panics hands the task, and the
//! startup refresh, to the port's handler, which does the same.

use std::path::PathBuf;
use std::sync::Arc;

use bake_agent::QueueMode;
use bake_ai::{AbortSignal, Model, ModelThinkingLevel};
use bake_coding_agent::agent_session::{AgentSession, DefaultSave};
use bake_coding_agent::cliproxyapi::account::{
    AccountView, CliProxyAccount, RefreshOutcome, disabled_warning, refresh_account,
};
use bake_coding_agent::cliproxyapi::{
    CLIPROXYAPI_ID, ConfigureCliProxyError, LoginPrompt, LoginPromptKind, check_cli_proxy_login,
    cli_proxy_models_in_use,
};
use bake_coding_agent::settings::Settings;
use bake_tui_view::dialog::{
    DialogAnswer, DialogKind, DialogRequest, FieldDialog, ListDialog, ListItem,
};
use bake_tui_view::runtime::RuntimeUpdate;
use bake_tui_view::transcript::Row;
use tokio::sync::mpsc;

/// The commands this module runs, as typed after `/`.
pub const COMMANDS: &[&str] = &[
    "model",
    "thinking",
    "settings",
    "login",
    "logout",
    "refresh-models",
];

/// The command a prompt names, with its argument, when it names one of
/// [`COMMANDS`].
pub fn parse(text: &str) -> Option<(&'static str, String)> {
    let rest = text.trim().strip_prefix('/')?;
    let (word, argument) = match rest.split_once(char::is_whitespace) {
        Some((word, argument)) => (word, argument.trim()),
        None => (rest, ""),
    };
    let name = COMMANDS.iter().find(|name| **name == word)?;
    Some((name, argument.to_owned()))
}

/// Why a command ended without changing anything, for the draft's notice.
pub const CANCELLED: &str = "Cancelled; your draft is back.";

/// What a command works with.
pub struct Context {
    pub session: Arc<AgentSession>,
    pub home: Option<PathBuf>,
    pub send: Arc<dyn Fn(RuntimeUpdate) -> bool + Send + Sync>,
    pub answers: mpsc::UnboundedReceiver<(u64, DialogAnswer)>,
    pub signal: AbortSignal,
    /// The id of the last dialog, shared with every command, so no two
    /// dialogs share one.
    pub ids: Arc<std::sync::atomic::AtomicU64>,
    /// This command's dialog on screen, or 0 before it opens one and after
    /// it closes. It stays while the answer is worked on, so Esc there
    /// aborts this command.
    pub shown: Arc<std::sync::atomic::AtomicU64>,
    /// The asker once made, which then holds `answers`.
    shared: Option<Asker>,
    #[cfg(test)]
    commit_gate: Option<CommitGate>,
}

/// Holds `/login` once its check has passed, before its commit: the
/// command says so on `reached` and goes on when `release` lets it.
#[cfg(test)]
#[derive(Clone)]
pub struct CommitGate {
    pub reached: std::sync::mpsc::Sender<()>,
    pub release: Release,
}

/// What lets a [`CommitGate`] go on.
#[cfg(test)]
#[derive(Clone)]
pub enum Release {
    /// The `Notify` is notified.
    Notified(Arc<tokio::sync::Notify>),
    /// The command's abort signal fires.
    Aborted,
}

impl Context {
    /// A command's context.
    pub fn new(
        session: Arc<AgentSession>,
        home: Option<PathBuf>,
        send: Arc<dyn Fn(RuntimeUpdate) -> bool + Send + Sync>,
        answers: mpsc::UnboundedReceiver<(u64, DialogAnswer)>,
        signal: AbortSignal,
        ids: Arc<std::sync::atomic::AtomicU64>,
        shown: Arc<std::sync::atomic::AtomicU64>,
    ) -> Self {
        Self {
            session,
            home,
            send,
            answers,
            signal,
            ids,
            shown,
            shared: None,
            #[cfg(test)]
            commit_gate: None,
        }
    }

    #[cfg(test)]
    pub fn with_commit_gate(mut self, gate: Option<CommitGate>) -> Self {
        self.commit_gate = gate;
        self
    }
}

/// A command's result: a note for the transcript, or why it changed
/// nothing (its draft then comes back).
pub type Outcome = Result<String, String>;

/// What opens dialogs and takes their answers, for a command's prompts.
#[derive(Clone)]
struct Asker {
    send: Arc<dyn Fn(RuntimeUpdate) -> bool + Send + Sync>,
    answers: Arc<tokio::sync::Mutex<mpsc::UnboundedReceiver<(u64, DialogAnswer)>>>,
    ids: Arc<std::sync::atomic::AtomicU64>,
    shown: Arc<std::sync::atomic::AtomicU64>,
    signal: AbortSignal,
}

/// The next dialog id. Ids only grow, so an answer for a closed dialog
/// never names a later one; at the end of the range there is none.
pub fn next_dialog_id(ids: &std::sync::atomic::AtomicU64) -> Option<u64> {
    use std::sync::atomic::Ordering;
    ids.try_update(Ordering::SeqCst, Ordering::SeqCst, |last| {
        last.checked_add(1)
    })
    .ok()
    .and_then(|last| last.checked_add(1))
}

impl Asker {
    /// Opens `kind` and waits for its answer; answers for other dialogs
    /// are dropped. A closed channel or an aborted signal reads as
    /// [`DialogAnswer::Cancel`].
    async fn ask(self, kind: DialogKind) -> DialogAnswer {
        let Some(id) = next_dialog_id(&self.ids) else {
            return DialogAnswer::Cancel;
        };
        self.shown.store(id, std::sync::atomic::Ordering::SeqCst);
        (self.send)(RuntimeUpdate::Dialog(Some(DialogRequest { id, kind })));
        let mut answers = self.answers.lock().await;
        loop {
            if self.signal.aborted() {
                return DialogAnswer::Cancel;
            }
            tokio::select! {
                () = self.signal.cancelled() => return DialogAnswer::Cancel,
                answer = answers.recv() => match answer {
                    Some((answered, answer)) if answered == id => return answer,
                    Some(_) => {}
                    None => return DialogAnswer::Cancel,
                },
            }
        }
    }
}

impl Context {
    fn asker(&mut self) -> Asker {
        let answers = std::mem::replace(&mut self.answers, mpsc::unbounded_channel().1);
        let asker = self.shared.get_or_insert_with(|| Asker {
            send: Arc::clone(&self.send),
            answers: Arc::new(tokio::sync::Mutex::new(answers)),
            ids: Arc::clone(&self.ids),
            shown: Arc::clone(&self.shown),
            signal: self.signal.clone(),
        });
        asker.clone()
    }

    async fn ask(&mut self, kind: DialogKind) -> DialogAnswer {
        self.asker().ask(kind).await
    }

    fn account(&self) -> Result<CliProxyAccount, String> {
        self.home
            .as_deref()
            .map(CliProxyAccount::new)
            .ok_or_else(|| "Cannot find the Bake home, so there is no account to change.".into())
    }
}

/// Why a command refuses `argument` before doing anything: `/login`,
/// `/logout`, and `/refresh-models` know CLIProxyAPI alone, and
/// `/settings` takes no argument.
pub fn refuse_argument(name: &str, argument: &str) -> Option<String> {
    match name {
        "login" | "logout" if !argument.is_empty() && argument != CLIPROXYAPI_ID => Some(format!(
            "/{name} knows only CLIProxyAPI in bake-rs, not {argument}; your draft is kept."
        )),
        "refresh-models" | "settings" if !argument.is_empty() => {
            Some(format!("/{name} takes no argument; your draft is kept."))
        }
        _ => None,
    }
}

/// Runs `name` with `argument`.
pub async fn run(name: &str, argument: String, mut context: Context) -> Outcome {
    if let Some(reason) = refuse_argument(name, &argument) {
        return Err(reason);
    }
    let outcome = match name {
        "model" => model(&mut context, &argument).await,
        "thinking" => thinking(&mut context, &argument).await,
        "settings" => settings(&mut context).await,
        "login" => login(&mut context).await,
        "logout" => logout(&context).await,
        "refresh-models" => refresh(&context).await,
        _ => Err(format!("/{name} is not a command.")),
    };
    context.shown.store(0, std::sync::atomic::Ordering::SeqCst);
    (context.send)(RuntimeUpdate::Dialog(None));
    outcome
}

fn model_ref(model: &Model) -> String {
    format!("{}/{}", model.provider, model.id)
}

fn saved(what: &str, save: &DefaultSave) -> String {
    match save {
        DefaultSave::NotRequested => what.to_owned(),
        DefaultSave::Saved => format!("{what}, saved as the default"),
        DefaultSave::Unsynced(reason) => {
            format!("{what}, saved as the default (it may not survive a power loss: {reason})")
        }
        DefaultSave::Failed(reason) => {
            format!("{what}; the default could not be saved: {reason}")
        }
    }
}

/// Pi's `findExactModelMatch` over the models with keys: `provider/id`, or
/// an id only one provider has.
fn exact_model(models: &[Model], wanted: &str) -> Option<Model> {
    if let Some((provider, id)) = wanted.split_once('/')
        && let Some(model) = models.iter().find(|m| m.provider == provider && m.id == id)
    {
        return Some(model.clone());
    }
    let mut by_id = models.iter().filter(|m| m.id == wanted);
    match (by_id.next(), by_id.next()) {
        (Some(model), None) => Some(model.clone()),
        _ => None,
    }
}

async fn model(context: &mut Context, argument: &str) -> Outcome {
    let models = context.session.model_runtime().snapshot().available();
    if models.is_empty() {
        return Err("No model has a key yet. Run /login to sign in to CLIProxyAPI.".into());
    }
    let (chosen, save) = match exact_model(&models, argument) {
        Some(model) => (model, false),
        None => {
            let current = context.session.model();
            let items = models
                .iter()
                .map(|model| ListItem {
                    value: model_ref(model),
                    label: model.id.clone(),
                    detail: Some(model.provider.clone()),
                })
                .collect();
            let current = current.and_then(|current| {
                models
                    .iter()
                    .position(|m| m.provider == current.provider && m.id == current.id)
            });
            let answer = context
                .ask(DialogKind::List(ListDialog {
                    title: "Model".into(),
                    items,
                    current,
                    filterable: true,
                    saves_default: true,
                    note: (!argument.is_empty())
                        .then(|| format!("No model is named {argument}; choose one.")),
                }))
                .await;
            let DialogAnswer::Chose {
                value,
                save_default,
            } = answer
            else {
                return Err(CANCELLED.into());
            };
            let Some(model) = models.iter().find(|m| model_ref(m) == value).cloned() else {
                return Err(CANCELLED.into());
            };
            (model, save_default)
        }
    };
    let name = model_ref(&chosen);
    let result = context.session.set_model(chosen, save).await?;
    Ok(saved(&format!("Model: {name}"), &result))
}

async fn thinking(context: &mut Context, argument: &str) -> Outcome {
    let levels = context.session.available_thinking_levels();
    let named = levels
        .iter()
        .copied()
        .find(|level| level.as_str() == argument);
    let (level, save) = match named {
        Some(level) => (level, false),
        None => {
            let current = context.session.thinking_level();
            let answer = context
                .ask(DialogKind::List(ListDialog {
                    title: "Thinking level".into(),
                    items: levels
                        .iter()
                        .map(|level| ListItem {
                            value: level.as_str().into(),
                            label: level.as_str().into(),
                            detail: None,
                        })
                        .collect(),
                    current: levels.iter().position(|level| *level == current),
                    filterable: false,
                    saves_default: true,
                    note: (!argument.is_empty())
                        .then(|| format!("This model has no thinking level {argument}.")),
                }))
                .await;
            let DialogAnswer::Chose {
                value,
                save_default,
            } = answer
            else {
                return Err(CANCELLED.into());
            };
            let Some(level) = levels.iter().copied().find(|l| l.as_str() == value) else {
                return Err(CANCELLED.into());
            };
            (level, save_default)
        }
    };
    let result = context.session.set_thinking_level(level, save).await?;
    let applied: ModelThinkingLevel = context.session.thinking_level();
    Ok(saved(
        &format!("Thinking level: {}", applied.as_str()),
        &result,
    ))
}

fn queue_mode(mode: QueueMode) -> &'static str {
    match mode {
        QueueMode::All => "all",
        QueueMode::OneAtATime => "one-at-a-time",
    }
}

fn on_off(on: bool) -> &'static str {
    if on { "on" } else { "off" }
}

async fn settings(context: &mut Context) -> Outcome {
    let session = Arc::clone(&context.session);
    let compaction = session.settings().compaction_enabled();
    let rows = [
        (
            "steering",
            "Steering mode",
            queue_mode(session.steering_mode()),
        ),
        (
            "follow-up",
            "Follow-up mode",
            queue_mode(session.follow_up_mode()),
        ),
        ("compaction", "Auto-compaction", on_off(compaction)),
    ];
    let answer = context
        .ask(DialogKind::List(ListDialog {
            title: "Settings".into(),
            items: rows
                .iter()
                .map(|(value, label, now)| ListItem {
                    value: (*value).into(),
                    label: (*label).into(),
                    detail: Some((*now).into()),
                })
                .collect(),
            current: None,
            filterable: false,
            saves_default: false,
            note: Some("Saved to settings.json; applies now.".into()),
        }))
        .await;
    let DialogAnswer::Chose { value: setting, .. } = answer else {
        return Err(CANCELLED.into());
    };
    let (title, choices, now): (&str, &[&str], &str) = match setting.as_str() {
        "steering" => (
            "Steering mode",
            &["one-at-a-time", "all"],
            queue_mode(session.steering_mode()),
        ),
        "follow-up" => (
            "Follow-up mode",
            &["one-at-a-time", "all"],
            queue_mode(session.follow_up_mode()),
        ),
        "compaction" => ("Auto-compaction", &["on", "off"], on_off(compaction)),
        _ => return Err(CANCELLED.into()),
    };
    let answer = context
        .ask(DialogKind::List(ListDialog {
            title: title.into(),
            items: choices
                .iter()
                .map(|choice| ListItem {
                    value: (*choice).into(),
                    label: (*choice).into(),
                    detail: None,
                })
                .collect(),
            current: choices.iter().position(|choice| *choice == now),
            filterable: false,
            saves_default: false,
            note: None,
        }))
        .await;
    let DialogAnswer::Chose { value, .. } = answer else {
        return Err(CANCELLED.into());
    };
    let mode = |value: &str| {
        if value == "all" {
            QueueMode::All
        } else {
            QueueMode::OneAtATime
        }
    };
    let result = match setting.as_str() {
        "steering" => session.set_steering_mode(mode(&value)).await?,
        "follow-up" => session.set_follow_up_mode(mode(&value)).await?,
        _ => session.set_auto_compaction_enabled(value == "on").await?,
    };
    let note = format!("{title}: {value}");
    Ok(match result {
        DefaultSave::Failed(reason) => {
            format!("{note} for this session; it could not be saved: {reason}")
        }
        DefaultSave::Unsynced(reason) => {
            format!("{note} (saved; it may not survive a power loss: {reason})")
        }
        _ => note,
    })
}

/// The base URL the signed-in account uses, to offer again.
fn saved_base_url(account: &AccountView) -> Option<String> {
    let AccountView::Bound(bound) = account else {
        return None;
    };
    bound
        .route
        .get("baseURL")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
}

async fn login(context: &mut Context) -> Outcome {
    let account = context.account()?;
    let (account, view, manual) = {
        let account = account.clone();
        tokio::task::spawn_blocking(move || {
            let view = account.read();
            let manual = account.manual_override();
            (account, view, manual)
        })
        .await
        .map_err(|error| error.to_string())?
    };
    if manual {
        return Err(
            "models.json configures the cliproxyapi provider; edit it there, or remove it to sign in here."
                .into(),
        );
    }
    let saved = saved_base_url(&view);
    let signal = context.signal.clone();
    let asker = context.asker();
    let checked = {
        check_cli_proxy_login(
            saved.as_deref(),
            |prompt: LoginPrompt| {
                let secret = prompt.kind == LoginPromptKind::Secret;
                let mut message = format!(
                    "{} ({}/{})",
                    prompt.message, prompt.step.index, prompt.step.count
                );
                if let Some(hint) = &prompt.hint {
                    message.push_str(&format!(" · {hint}"));
                }
                // A secret is never sent back to the view: a retried key
                // step starts empty. The address step shows what was typed,
                // or the address used when nothing is.
                let initial = if secret {
                    String::new()
                } else {
                    prompt
                        .initial
                        .clone()
                        .or_else(|| prompt.fallback.clone())
                        .unwrap_or_default()
                };
                let kind = DialogKind::Field(FieldDialog {
                    title: prompt.title.clone(),
                    prompt: message,
                    secret,
                    initial,
                    error: prompt.error.clone(),
                });
                let asked = asker.clone().ask(kind);
                async move {
                    match asked.await {
                        DialogAnswer::Entered(text) => Ok(text.expose().to_owned()),
                        _ => Err(()),
                    }
                }
            },
            Some(&signal),
            |_host| {},
        )
        .await
    };
    let checked = match checked {
        Ok(checked) => checked,
        Err(ConfigureCliProxyError::Declined(()) | ConfigureCliProxyError::Aborted) => {
            return Err(CANCELLED.into());
        }
        Err(error) => return Err(format!("Sign-in failed: {}", describe(&error))),
    };
    // From here the sign-in is committed or reported, never abandoned: the
    // abort signal is not read again.
    #[cfg(test)]
    if let Some(gate) = &context.commit_gate {
        let _ = gate.reached.send(());
        match &gate.release {
            Release::Notified(release) => release.notified().await,
            Release::Aborted => context.signal.cancelled().await,
        }
    }
    let runtime = context.session.model_runtime().clone();
    let committed = tokio::task::spawn_blocking(move || {
        let committed = account.commit_login(&checked);
        // Whatever the files now hold reaches the running session.
        let applied = runtime.try_update(|registry| account.apply_to(registry));
        (committed, applied)
    })
    .await
    .map_err(|error| error.to_string())?;
    let (committed, applied) = committed;
    let committed = committed.map_err(|error| format!("Sign-in was not saved: {error}"))?;
    let mut note = format!(
        "Signed in to CLIProxyAPI · {} model{} ready",
        committed.models,
        if committed.models == 1 { "" } else { "s" }
    );
    match applied {
        Ok(Some(warning)) | Err(warning) => note.push_str(&format!(" · {warning}")),
        Ok(None) => {}
    }
    for warning in &committed.warnings {
        note.push_str(&format!(" · {UNSYNCED}: {warning}"));
    }
    // Pi's selector keeps a usable model; without one, the route's first.
    if context.session.check_model_auth().is_err() {
        let first = context
            .session
            .model_runtime()
            .snapshot()
            .available()
            .into_iter()
            .find(|model| model.provider == CLIPROXYAPI_ID);
        if let Some(first) = first {
            let name = model_ref(&first);
            match context.session.set_model(first, false).await {
                Ok(_) => note.push_str(&format!(" · now using {name}")),
                Err(error) => note.push_str(&format!(" · could not switch to {name}: {error}")),
            }
        }
    }
    Ok(note)
}

/// A sign-in failure's text. It names no key: the setup's errors carry
/// none.
fn describe(error: &ConfigureCliProxyError<()>) -> String {
    match error {
        ConfigureCliProxyError::InvalidSavedUrl(error) => {
            format!("the saved address is invalid ({error:?})")
        }
        ConfigureCliProxyError::CredentialRead(reason)
        | ConfigureCliProxyError::CredentialWrite(reason)
        | ConfigureCliProxyError::SettingsWrite(reason) => reason.clone(),
        ConfigureCliProxyError::RollbackFailed {
            settings_error,
            rollback_error,
        } => format!("{settings_error}; {rollback_error}"),
        ConfigureCliProxyError::Aborted | ConfigureCliProxyError::Declined(()) => CANCELLED.into(),
    }
}

async fn logout(context: &Context) -> Outcome {
    let account = context.account()?;
    let runtime = context.session.model_runtime().clone();
    let (out, applied, manual) = tokio::task::spawn_blocking(move || {
        let out = account.logout();
        let applied = runtime.try_update(|registry| account.apply_to(registry));
        (out, applied, account.manual_override())
    })
    .await
    .map_err(|error| error.to_string())?;
    let out = out.map_err(|error| format!("Sign-out failed: {error}"))?;
    let mut note = "Signed out of CLIProxyAPI".to_owned();
    if let Some(error) = &out.key_error {
        note.push_str(&format!(
            " · the key could not be removed from auth.json ({error}), but nothing uses it"
        ));
    }
    if let Err(error) = applied {
        note.push_str(&format!(" · {error}"));
    }
    for warning in &out.warnings {
        note.push_str(&format!(" · may not survive a power loss: {warning}"));
    }
    if manual {
        note.push_str(" · models.json still configures cliproxyapi; that is yours to change");
    }
    if context
        .session
        .model()
        .is_some_and(|model| model.provider == CLIPROXYAPI_ID)
        && context.session.check_model_auth().is_err()
    {
        note.push_str(" · choose another model with /model, or sign in again with /login");
    }
    Ok(note)
}

/// Bake's `cliProxyModelsInUse` for this session: the current model and
/// the new-session default, which a refresh keeps even when the proxy no
/// longer lists them. `bake-rs` has no subagent allow-list.
fn models_in_use(current: Option<&Model>, settings: &Settings) -> Vec<String> {
    in_use(
        current.map(|model| (model.provider.as_str(), model.id.as_str())),
        settings,
    )
}

fn in_use(current: Option<(&str, &str)>, settings: &Settings) -> Vec<String> {
    let default = settings
        .default_provider
        .as_deref()
        .zip(settings.default_model.as_deref());
    cli_proxy_models_in_use(current, default, None)
}

async fn refresh(context: &Context) -> Outcome {
    let account = context.account()?;
    let keep = models_in_use(
        context.session.model().as_ref(),
        &context.session.settings(),
    );
    let refreshed = refresh_account(
        account,
        context.session.model_runtime().clone(),
        keep,
        Some(context.signal.clone()),
    )
    .await
    .map_err(|error| {
        if context.signal.aborted() {
            CANCELLED.to_owned()
        } else {
            format!("The model list was not refreshed: {error}")
        }
    })?;
    let mut note = match refreshed.outcome {
        RefreshOutcome::NotSignedIn => {
            return Err("There is no CLIProxyAPI sign-in to refresh. Run /login first.".into());
        }
        RefreshOutcome::Disabled(reason) => return Err(disabled_warning(&reason)),
        RefreshOutcome::Unchanged => "The CLIProxyAPI model list is up to date".to_owned(),
        RefreshOutcome::Updated(count) => format!(
            "Refreshed the CLIProxyAPI model list · {count} model{}",
            if count == 1 { "" } else { "s" }
        ),
    };
    for warning in &refreshed.warnings {
        note.push_str(&format!(" · {warning}"));
    }
    for warning in &refreshed.unsynced {
        note.push_str(&format!(" · {UNSYNCED}: {warning}"));
    }
    Ok(note)
}

/// What a note says of a committed write whose directory flush failed.
const UNSYNCED: &str = "may not survive a power loss";

/// The once-per-launch refresh, Bake's `runner.ts` background
/// `refreshCliProxyModels`: only a signed-in account, keeping the models in
/// use ([`models_in_use`]); bounded by `CATALOG_TIMEOUT`'s fetch and the
/// abort signal. It is quiet, as Bake's is, except that a route write it
/// committed but could not flush goes to the transcript through `send`.
pub async fn startup_refresh(
    session: Arc<AgentSession>,
    home: PathBuf,
    signal: AbortSignal,
    send: Arc<dyn Fn(RuntimeUpdate) -> bool + Send + Sync>,
) {
    let keep = models_in_use(session.model().as_ref(), &session.settings());
    let account = CliProxyAccount::new(&home);
    let Ok(refreshed) =
        refresh_account(account, session.model_runtime().clone(), keep, Some(signal)).await
    else {
        return;
    };
    if !refreshed.unsynced.is_empty() {
        let rows = refreshed
            .unsynced
            .iter()
            .map(|warning| {
                Row::Info(format!(
                    "The CLIProxyAPI model list was refreshed but {UNSYNCED}: {warning}"
                ))
            })
            .collect();
        send(RuntimeUpdate::Commit(rows));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_cliproxyapi_is_signed_in_or_out() {
        assert_eq!(refuse_argument("login", ""), None);
        assert_eq!(refuse_argument("logout", "cliproxyapi"), None);
        assert!(refuse_argument("logout", "openai").is_some());
        assert!(refuse_argument("login", "anthropic").is_some());
        assert!(refuse_argument("refresh-models", "x").is_some());
        assert_eq!(refuse_argument("model", "faux/faux-1"), None);
    }

    // cliproxyapi.test.ts "keeps the models in use": the current model and
    // the new-session default, each once, CLIProxyAPI's only.
    #[test]
    fn a_refresh_keeps_the_current_and_default_models() {
        let mut settings = Settings {
            default_provider: Some(CLIPROXYAPI_ID.into()),
            default_model: Some("gpt-default".into()),
            ..Settings::default()
        };
        assert_eq!(
            in_use(Some((CLIPROXYAPI_ID, "gpt-a")), &settings),
            ["gpt-a", "gpt-default"]
        );
        assert_eq!(in_use(Some(("faux", "gpt-a")), &settings), ["gpt-default"]);
        settings.default_provider = Some("faux".into());
        assert!(in_use(None, &settings).is_empty());
    }

    #[test]
    fn dialog_ids_only_grow_and_end_rather_than_wrap() {
        let ids = std::sync::atomic::AtomicU64::new(u64::MAX - 1);
        assert_eq!(next_dialog_id(&ids), Some(u64::MAX));
        assert_eq!(next_dialog_id(&ids), None);
        assert_eq!(next_dialog_id(&ids), None);
    }

    #[test]
    fn commands_parse_with_their_argument() {
        assert_eq!(parse("/model faux-2"), Some(("model", "faux-2".into())));
        assert_eq!(parse(" /login "), Some(("login", String::new())));
        assert_eq!(
            parse("/refresh-models"),
            Some(("refresh-models", String::new()))
        );
        assert_eq!(parse("/models"), None);
        assert_eq!(parse("model"), None);
    }
}
