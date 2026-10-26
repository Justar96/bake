//! `/settings`: the settings a running session applies at once, grouped
//! under the slash command or the part of the screen whose work they
//! shape, as Pi v1.4.0's
//! `settings-selector.ts` lists one row per setting with its value and a
//! description.
//!
//! Each row shows its value, and the selected row's description shows
//! under the list. Enter opens the row's values, the current one marked;
//! choosing one applies it now, saves it to `settings.json`, and comes back
//! to the list, as Pi's selector stays open, and Esc there goes back
//! without a change. Esc on the list closes the sheet: with the changes
//! made listed in the transcript, or, without any, with the draft back.

use bake_agent::QueueMode;
use bake_coding_agent::agent_session::{AgentSession, DefaultSave};
use bake_coding_agent::compaction::{CompactionSettings, DEFAULT_COMPACTION_SETTINGS};
use bake_tui_view::dialog::{DialogAnswer, DialogKind, ListDialog, ListItem};
use bake_tui_view::transcript::Row;

use super::{CANCELLED, Context, Done, Outcome, tokens};

/// The settings `/compact` and automatic compaction follow.
pub const COMPACTION: &str = "Compaction · /compact";
/// The settings of prompts sent while a turn runs.
pub const QUEUE: &str = "Prompts sent during a turn";
/// The settings `/tree` follows.
pub const TREE: &str = "Branches · /tree";
/// What the status line under the composer shows.
pub const STATUS_LINE: &str = "Status line";

/// What the sheet shows, read when it opens and after each change.
#[derive(Debug, Clone, PartialEq)]
pub struct View {
    pub auto: bool,
    pub cached: bool,
    /// The compaction settings for the session's model, or why they are
    /// invalid; the values shown are then the defaults.
    pub compaction: Result<CompactionSettings, String>,
    /// The model's context window; 0 without a model.
    pub window: u64,
    pub steering: QueueMode,
    pub follow_up: QueueMode,
    pub skip_branch_summary: bool,
    /// The status line's token totals and prompt cache.
    pub status_tokens: bool,
    pub status_cache: bool,
}

impl View {
    fn of(session: &AgentSession) -> Self {
        let settings = session.settings();
        let model = session.model();
        Self {
            auto: settings.compaction_enabled(),
            cached: settings.cached_compaction(),
            compaction: settings.compaction_settings(
                model
                    .as_ref()
                    .map(|model| (model.provider.as_str(), model.id.as_str())),
            ),
            window: model.map_or(0, |model| model.context_window),
            steering: session.steering_mode(),
            follow_up: session.follow_up_mode(),
            skip_branch_summary: settings.branch_summary_skip_prompt,
            status_tokens: settings.status_line.tokens,
            status_cache: settings.status_line.cache,
        }
    }

    fn resolved(&self) -> CompactionSettings {
        self.compaction
            .clone()
            .unwrap_or(DEFAULT_COMPACTION_SETTINGS)
    }

    /// Where automatic compaction starts with `percent`, as `120k of 200k`.
    fn starts_at(&self, percent: Option<u8>) -> Option<String> {
        let settings = CompactionSettings {
            threshold_percent: percent,
            ..self.resolved()
        };
        let at = u64::try_from(settings.threshold(self.window)).ok()?;
        (self.window > 0).then(|| format!("{} of {}", tokens(at), tokens(self.window)))
    }
}

fn on_off(on: bool) -> &'static str {
    if on { "on" } else { "off" }
}

fn queue_mode(mode: QueueMode) -> &'static str {
    match mode {
        QueueMode::All => "all",
        QueueMode::OneAtATime => "one-at-a-time",
    }
}

fn percent(percent: Option<u8>) -> String {
    percent.map_or_else(|| "default".to_owned(), |percent| format!("{percent}%"))
}

/// The settings' rows, grouped, each with its value and description.
pub fn rows(view: &View) -> Vec<ListItem> {
    let resolved = view.resolved();
    let invalid = view.compaction.is_err();
    let compact_at = match view.starts_at(resolved.threshold_percent) {
        Some(at) if view.auto => format!("Auto-compaction starts past {at} tokens."),
        _ if view.auto => "When auto-compaction starts, as a share of the context window.".into(),
        _ => "When auto-compaction starts; it is off now.".into(),
    };
    let row = |value: &str, group: &str, label: &str, now: String, about: String| ListItem {
        value: value.into(),
        label: label.into(),
        group: Some(group.into()),
        detail: Some(now),
        about: Some(about),
        ..ListItem::default()
    };
    let compaction_value = |shown: String| {
        if invalid {
            "invalid setting".to_owned()
        } else {
            shown
        }
    };
    vec![
        row(
            "compaction",
            COMPACTION,
            "Auto-compaction",
            on_off(view.auto).into(),
            "Compact as the context fills up; /compact works either way.".into(),
        ),
        row(
            "compact-at",
            COMPACTION,
            "Compact at",
            compaction_value(percent(resolved.threshold_percent)),
            view.compaction.clone().err().unwrap_or(compact_at),
        ),
        row(
            "keep-recent",
            COMPACTION,
            "Keep recent",
            compaction_value(format!("{} tokens", tokens(resolved.keep_recent_tokens))),
            "Recent conversation a compaction keeps word for word.".into(),
        ),
        row(
            "cached-summary",
            COMPACTION,
            "Cached summary",
            on_off(view.cached).into(),
            "Summarize from the provider's prompt cache, with full tool output.".into(),
        ),
        row(
            "steering",
            QUEUE,
            "Steering mode",
            queue_mode(view.steering).into(),
            "Prompts sent mid-turn: one at a time, or all at the next step.".into(),
        ),
        row(
            "follow-up",
            QUEUE,
            "Follow-up mode",
            queue_mode(view.follow_up).into(),
            "Follow-ups wait for the turn to end: one at a time, or all at once.".into(),
        ),
        row(
            "branch-summary",
            TREE,
            "Branch summary",
            if view.skip_branch_summary {
                "skip"
            } else {
                "ask"
            }
            .into(),
            "Whether /tree asks to summarize the branch it leaves.".into(),
        ),
        row(
            "status-tokens",
            STATUS_LINE,
            "Token totals",
            on_off(view.status_tokens).into(),
            "The session's input ↑ and output ↓ tokens, after the context.".into(),
        ),
        row(
            "status-cache",
            STATUS_LINE,
            "Prompt cache",
            on_off(view.status_cache).into(),
            "The latest prompt's cache hit, then the cache's reads and writes.".into(),
        ),
    ]
}

/// The values of one setting.
#[derive(Debug, Clone, PartialEq)]
pub struct Choices {
    pub title: &'static str,
    pub items: Vec<ListItem>,
    pub current: Option<usize>,
    pub note: Option<String>,
}

/// The thresholds `Compact at` offers, in percent of the context window.
const THRESHOLDS: [u8; 8] = [50, 60, 70, 75, 80, 85, 90, 95];

/// The tokens `Keep recent` offers.
const KEPT: [u64; 6] = [5_000, 10_000, 20_000, 30_000, 40_000, 60_000];

fn choice(value: impl Into<String>, label: impl Into<String>, fact: Option<String>) -> ListItem {
    ListItem {
        value: value.into(),
        label: label.into(),
        facts: fact.into_iter().collect(),
        ..ListItem::default()
    }
}

fn simple(title: &'static str, values: &[&str], now: &str, note: Option<String>) -> Choices {
    Choices {
        title,
        items: values
            .iter()
            .map(|value| choice(*value, *value, None))
            .collect(),
        current: values.iter().position(|value| *value == now),
        note,
    }
}

/// The values of `setting`, the current one marked: a value the file
/// holds that the list does not offer is listed in its place.
pub fn choices(setting: &str, view: &View) -> Option<Choices> {
    let resolved = view.resolved();
    Some(match setting {
        "compaction" => simple("Auto-compaction", &["on", "off"], on_off(view.auto), None),
        "status-tokens" => simple("Token totals", &["on", "off"], on_off(view.status_tokens), None),
        "status-cache" => simple(
            "Prompt cache",
            &["on", "off"],
            on_off(view.status_cache),
            Some("cache 99% (R180k W2k): the share of the latest prompt read from the cache, then the session's cache reads and writes.".into()),
        ),
        "cached-summary" => simple(
            "Cached summary",
            &["on", "off"],
            on_off(view.cached),
            Some(
                "Repeats the conversation the provider cached and asks for the summary; Pi's summary runs when the cache cannot be read."
                    .into(),
            ),
        ),
        "steering" => simple(
            "Steering mode",
            &["one-at-a-time", "all"],
            queue_mode(view.steering),
            None,
        ),
        "follow-up" => simple(
            "Follow-up mode",
            &["one-at-a-time", "all"],
            queue_mode(view.follow_up),
            None,
        ),
        "branch-summary" => simple(
            "Branch summary",
            &["ask", "skip"],
            if view.skip_branch_summary {
                "skip"
            } else {
                "ask"
            },
            None,
        ),
        "compact-at" => {
            let mut offered: Vec<u8> = THRESHOLDS.to_vec();
            if let Some(now) = resolved.threshold_percent
                && !offered.contains(&now)
            {
                offered.push(now);
                offered.sort_unstable();
            }
            let mut items: Vec<ListItem> = offered
                .iter()
                .map(|at| {
                    choice(
                        at.to_string(),
                        format!("{at}%"),
                        view.starts_at(Some(*at)),
                    )
                })
                .collect();
            items.push(choice(
                "default",
                format!(
                    "Default: leave {} free",
                    tokens(resolved.reserve_tokens)
                ),
                view.starts_at(None),
            ));
            let current = match resolved.threshold_percent {
                Some(now) => offered.iter().position(|at| *at == now),
                None => Some(items.len() - 1),
            };
            Choices {
                title: "Compact at",
                items,
                current,
                note: Some(
                    "Auto-compaction starts once the context passes this share of the model's window."
                        .into(),
                ),
            }
        }
        "keep-recent" => {
            let mut offered: Vec<u64> = KEPT.to_vec();
            if !offered.contains(&resolved.keep_recent_tokens) {
                offered.push(resolved.keep_recent_tokens);
                offered.sort_unstable();
            }
            let default = DEFAULT_COMPACTION_SETTINGS.keep_recent_tokens;
            let items = offered
                .iter()
                .map(|kept| {
                    if *kept == default {
                        choice("default", format!("{} tokens (default)", tokens(*kept)), None)
                    } else {
                        choice(kept.to_string(), format!("{} tokens", tokens(*kept)), None)
                    }
                })
                .collect();
            Choices {
                title: "Keep recent",
                items,
                current: offered
                    .iter()
                    .position(|kept| *kept == resolved.keep_recent_tokens),
                note: None,
            }
        }
        _ => return None,
    })
}

fn mode(value: &str) -> QueueMode {
    if value == "all" {
        QueueMode::All
    } else {
        QueueMode::OneAtATime
    }
}

/// Applies `value` to `setting` and saves it.
async fn apply(session: &AgentSession, setting: &str, value: &str) -> Result<DefaultSave, String> {
    let number = |value: &str| value.parse::<u64>().ok();
    match setting {
        "compaction" => session.set_auto_compaction_enabled(value == "on").await,
        "cached-summary" => session.set_cached_compaction(value == "on").await,
        "steering" => session.set_steering_mode(mode(value)).await,
        "follow-up" => session.set_follow_up_mode(mode(value)).await,
        "branch-summary" => {
            session
                .set_branch_summary_skip_prompt(value == "skip")
                .await
        }
        "compact-at" => {
            let percent = number(value).and_then(|percent| u8::try_from(percent).ok());
            session.set_compaction_threshold_percent(percent).await
        }
        "keep-recent" => {
            session
                .set_compaction_keep_recent_tokens(number(value))
                .await
        }
        "status-tokens" => session.set_status_line_tokens(value == "on").await,
        "status-cache" => session.set_status_line_cache(value == "on").await,
        _ => Err(format!("{setting} is not a setting.")),
    }
}

fn saved(note: String, result: DefaultSave) -> String {
    match result {
        DefaultSave::Failed(reason) => {
            format!("{note} for this session; it could not be saved: {reason}")
        }
        DefaultSave::Unsynced(reason) => {
            format!("{note} (saved; it may not survive a power loss: {reason})")
        }
        _ => note,
    }
}

/// `/settings`: the grouped list, then each chosen setting's values, until
/// Esc closes the list.
pub async fn settings(context: &mut Context) -> Outcome {
    let session = std::sync::Arc::clone(&context.session);
    let mut changes: Vec<String> = Vec::new();
    let mut selected = None;
    loop {
        let view = View::of(&session);
        let items = rows(&view);
        let answer = context
            .ask(DialogKind::List(ListDialog {
                title: "Settings".into(),
                items: items.clone(),
                current: None,
                selected,
                filterable: false,
                saves_default: false,
                note: Some("Saved to settings.json; applies now.".into()),
                search: None,
            }))
            .await;
        let DialogAnswer::Chose { value: setting, .. } = answer else {
            break;
        };
        selected = items.iter().position(|item| item.value == setting);
        let Some(choices) = choices(&setting, &view) else {
            break;
        };
        let answer = context
            .ask_back(DialogKind::List(ListDialog {
                title: choices.title.into(),
                items: choices.items.clone(),
                current: choices.current,
                selected: None,
                filterable: false,
                saves_default: false,
                note: choices.note.clone(),
                search: None,
            }))
            .await;
        let DialogAnswer::Chose { value, .. } = answer else {
            // Esc on the values goes back to the list; an abort ends it.
            if context.signal.aborted() {
                break;
            }
            continue;
        };
        let label = choices
            .items
            .iter()
            .find(|item| item.value == value)
            .map_or_else(|| value.clone(), |item| item.label.clone());
        let result = apply(&session, &setting, &value).await?;
        if setting.starts_with("status-") {
            // The line under the sheet shows the change at once.
            (context.send)(bake_tui_view::runtime::RuntimeUpdate::Status(
                crate::live::publish::read_status(&session),
            ));
        }
        changes.push(saved(format!("{}: {label}", choices.title), result));
        if context.signal.aborted() {
            break;
        }
    }
    match changes.len() {
        0 => Err(CANCELLED.into()),
        1 => Ok(Done::Note(changes.remove(0))),
        _ => Ok(Done::Rows(changes.into_iter().map(Row::Info).collect())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view() -> View {
        View {
            auto: true,
            cached: true,
            compaction: Ok(DEFAULT_COMPACTION_SETTINGS),
            window: 200_000,
            steering: QueueMode::OneAtATime,
            follow_up: QueueMode::All,
            skip_branch_summary: false,
            status_tokens: false,
            status_cache: true,
        }
    }

    // The rows sit under their command's heading, each with its value and
    // what it does.
    #[test]
    fn settings_are_grouped_by_the_command_they_shape() {
        let rows = rows(&view());
        let shown: Vec<(&str, &str, &str)> = rows
            .iter()
            .map(|row| {
                (
                    row.group.as_deref().unwrap_or(""),
                    row.label.as_str(),
                    row.detail.as_deref().unwrap_or(""),
                )
            })
            .collect();
        assert_eq!(
            shown,
            [
                (COMPACTION, "Auto-compaction", "on"),
                (COMPACTION, "Compact at", "default"),
                (COMPACTION, "Keep recent", "20k tokens"),
                (COMPACTION, "Cached summary", "on"),
                (QUEUE, "Steering mode", "one-at-a-time"),
                (QUEUE, "Follow-up mode", "all"),
                (TREE, "Branch summary", "ask"),
                (STATUS_LINE, "Token totals", "off"),
                (STATUS_LINE, "Prompt cache", "on"),
            ]
        );
        assert!(rows.iter().all(|row| row.about.is_some()));
        assert_eq!(
            rows[1].about.as_deref(),
            Some("Auto-compaction starts past 183k of 200k tokens.")
        );
        // Every row opens its values.
        for row in &rows {
            assert!(choices(&row.value, &view()).is_some(), "{}", row.value);
        }
        assert_eq!(choices("theme", &view()), None);
    }

    #[test]
    fn compact_at_offers_shares_of_the_window_and_pi_s_default() {
        let mut at_80 = view();
        at_80.compaction = Ok(CompactionSettings {
            threshold_percent: Some(80),
            ..DEFAULT_COMPACTION_SETTINGS
        });
        let choices = choices("compact-at", &at_80).expect("choices");
        let labels: Vec<&str> = choices
            .items
            .iter()
            .map(|item| item.label.as_str())
            .collect();
        assert_eq!(
            labels,
            [
                "50%",
                "60%",
                "70%",
                "75%",
                "80%",
                "85%",
                "90%",
                "95%",
                "Default: leave 16k free"
            ]
        );
        assert_eq!(choices.current, Some(4));
        assert_eq!(choices.items[4].facts, ["160k of 200k"]);
        // 95% would leave less than the reserve, which stays free.
        assert_eq!(choices.items[7].facts, ["183k of 200k"]);
        assert_eq!(choices.items[8].value, "default");
        assert_eq!(rows(&at_80)[1].detail.as_deref(), Some("80%"));
        // A value the list does not offer is listed in its place.
        let mut odd = view();
        odd.compaction = Ok(CompactionSettings {
            threshold_percent: Some(65),
            ..DEFAULT_COMPACTION_SETTINGS
        });
        let choices = choices_of("compact-at", &odd);
        assert_eq!(choices.items[2].label, "65%");
        assert_eq!(choices.current, Some(2));
        // Without a model there are no token figures.
        let mut no_model = view();
        no_model.window = 0;
        assert!(
            choices_of("compact-at", &no_model)
                .items
                .iter()
                .all(|item| item.facts.is_empty())
        );
    }

    fn choices_of(setting: &str, view: &View) -> Choices {
        choices(setting, view).expect("choices")
    }

    #[test]
    fn keep_recent_marks_the_default_and_the_current_value() {
        let choices = choices_of("keep-recent", &view());
        let labels: Vec<&str> = choices
            .items
            .iter()
            .map(|item| item.label.as_str())
            .collect();
        assert_eq!(
            labels,
            [
                "5k tokens",
                "10k tokens",
                "20k tokens (default)",
                "30k tokens",
                "40k tokens",
                "60k tokens"
            ]
        );
        assert_eq!(choices.items[2].value, "default");
        assert_eq!(choices.current, Some(2));
        let mut kept = view();
        kept.compaction = Ok(CompactionSettings {
            keep_recent_tokens: 25_000,
            ..DEFAULT_COMPACTION_SETTINGS
        });
        let choices = choices_of("keep-recent", &kept);
        assert_eq!(choices.items[3].label, "25k tokens");
        assert_eq!(choices.current, Some(3));
    }

    // An invalid compaction setting is shown as such, with its message.
    #[test]
    fn an_invalid_compaction_setting_is_shown() {
        let mut broken = view();
        broken.compaction = Err("Invalid compaction.thresholdPercent setting: 0.".into());
        let rows = rows(&broken);
        assert_eq!(rows[1].detail.as_deref(), Some("invalid setting"));
        assert_eq!(
            rows[1].about.as_deref(),
            Some("Invalid compaction.thresholdPercent setting: 0.")
        );
        let mut off = view();
        off.auto = false;
        assert_eq!(
            rows_about(&off, 1),
            "When auto-compaction starts; it is off now."
        );
    }

    fn rows_about(view: &View, index: usize) -> String {
        rows(view)[index].about.clone().unwrap_or_default()
    }

    /// The settings sheet as an 80-by-30 terminal draws it.
    fn screen(view: &View, down: usize) -> Vec<String> {
        use bake_tui_view::activity::Tones;
        use bake_tui_view::dialog::DialogRequest;
        use bake_tui_view::frame::FrameStyle;
        use bake_tui_view::keys::{Key, KeyInput, Mods};
        use bake_tui_view::runtime::RuntimeUpdate;
        use bake_tui_view::state::{Msg, State, update};
        let mut state = State::live(FrameStyle::default(), Tones::None, Vec::new());
        update(
            &mut state,
            Msg::Runtime(RuntimeUpdate::Dialog(Some(DialogRequest {
                id: 1,
                kind: DialogKind::List(ListDialog {
                    title: "Settings".into(),
                    items: rows(view),
                    current: None,
                    selected: None,
                    filterable: false,
                    saves_default: false,
                    note: Some("Saved to settings.json; applies now.".into()),
                    search: None,
                }),
            }))),
        );
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 30)).expect("terminal");
        let mut draw = |state: &mut State| {
            terminal
                .draw(|frame| bake_tui_view::render::render(state, frame))
                .expect("draw");
            let buffer = terminal.backend().buffer();
            buffer
                .content()
                .chunks(usize::from(buffer.area.width))
                .map(|row| {
                    row.iter()
                        .map(|cell| cell.symbol())
                        .collect::<String>()
                        .trim_end()
                        .to_owned()
                })
                .collect::<Vec<_>>()
        };
        draw(&mut state);
        if let Some(token) = state.approval_drawn() {
            update(&mut state, Msg::ApprovalArmed(token));
        }
        for _ in 0..down {
            update(
                &mut state,
                Msg::Key(KeyInput {
                    key: Key::Down,
                    mods: Mods::NONE,
                }),
            );
        }
        draw(&mut state)
    }

    // The sheet draws each group's heading over its rows, the values in one
    // column, and the selected row's description under the list.
    #[test]
    fn the_sheet_draws_groups_values_and_the_description() {
        let mut shown = view();
        shown.compaction = Ok(CompactionSettings {
            threshold_percent: Some(80),
            ..DEFAULT_COMPACTION_SETTINGS
        });
        let screen = screen(&shown, 1);
        let text = screen.join("\n");
        let top = screen
            .iter()
            .position(|line| line.contains("╭"))
            .unwrap_or_else(|| panic!("{text}"));
        assert_eq!(
            screen[top..top + 20],
            [
                "  ╭──────────────────────────────────────────────────────────────────────────╮",
                "  │ Settings                                                                 │",
                "  │ Saved to settings.json; applies now.                                     │",
                "  │ Compaction · /compact 4 ──────────────────────────────────────────────── │",
                "  │   Auto-compaction  on                                                    │",
                "  │ ▸ Compact at       80%                                                   │",
                "  │   Keep recent      20k tokens                                            │",
                "  │   Cached summary   on                                                    │",
                "  │ Prompts sent during a turn 2 ─────────────────────────────────────────── │",
                "  │   Steering mode    one-at-a-time                                         │",
                "  │   Follow-up mode   all                                                   │",
                "  │ Branches · /tree 1 ───────────────────────────────────────────────────── │",
                "  │   Branch summary   ask                                                   │",
                "  │ Status line 2 ────────────────────────────────────────────────────────── │",
                "  │   Token totals     off                                                   │",
                "  │   Prompt cache     on                                                    │",
                "  │   Auto-compaction starts past 160k of 200k tokens.                       │",
                "  │ ↑↓ choose · Enter select · Esc cancel                                2/9 │",
                "  ╰──────────────────────────────────────────────────────────────────────────╯",
                "                                                                      no model",
            ],
            "{text}"
        );
    }

    #[test]
    fn a_failed_save_is_reported_with_the_change() {
        assert_eq!(
            saved("Keep recent: 30k tokens".into(), DefaultSave::Saved),
            "Keep recent: 30k tokens"
        );
        assert_eq!(
            saved(
                "Cached summary: on".into(),
                DefaultSave::Failed("read-only".into())
            ),
            "Cached summary: on for this session; it could not be saved: read-only"
        );
    }
}
