//! A tool call's card: its name, target, and argument as the transcript
//! shows them, and its state, summary, and output from its result.

use super::*;

/// A tool call's card: its title, its main argument, the job or agent it
/// reached, and its outcome. A running `job_output` that waits says so.
pub(super) fn call_row(tool: &str, args: &Value, labels: &Labels, outcome: Outcome) -> Row {
    if tool == CODEMODE_TOOL_NAME {
        return Row::Codemode(Codemode {
            code: args
                .get("code")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            state: outcome.state,
            summary: outcome.summary,
            calls: outcome.calls,
            output: outcome.output,
            full_output_path: outcome.full_output_path,
            wall_time: outcome.wall_time,
        });
    }
    let waits = tool == "job_output" && args.get("wait").and_then(Value::as_bool) == Some(true);
    let summary = outcome.summary.or_else(|| {
        (outcome.state == CallState::Running && waits).then(|| copy::WAITING.to_owned())
    });
    Row::Call {
        tool: display_name(tool),
        argument: argument(tool, args, labels),
        id: outcome.id.or_else(|| target(tool, args)),
        state: outcome.state,
        summary,
        output: outcome.output,
    }
}

/// A tool's card title: its entry in [`copy::TOOL_TITLES`], or its name
/// with the first letter capitalized.
pub(super) fn display_name(tool: &str) -> String {
    if let Some((_, title)) = copy::TOOL_TITLES.iter().find(|(name, _)| *name == tool) {
        return (*title).to_owned();
    }
    let mut chars = tool.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// The background job or subagent a call names in its arguments.
pub(super) fn target(tool: &str, args: &Value) -> Option<String> {
    let key = match tool {
        "job_output" | "job_kill" => "job_id",
        "send_message" | "interrupt_agent" => "agent_id",
        _ => return None,
    };
    args.get(key)
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .map(str::to_owned)
}

/// The argument a card shows: the command, the path, or the pattern Pi's
/// tool renderers lead with; for a call that reaches a background job or
/// subagent, its label, since its id is tagged; other tools show their
/// arguments as JSON.
pub(super) fn argument(tool: &str, args: &Value, labels: &Labels) -> String {
    let field = |name: &str| args.get(name).and_then(Value::as_str).map(str::to_owned);
    let label = || {
        Some(
            target(tool, args)
                .and_then(|id| labels.get(&id).cloned())
                .unwrap_or_default(),
        )
    };
    let lead = match tool {
        "bash" => field("command"),
        "read" | "write" | "edit" => field("path"),
        "ls" => Some(field("path").unwrap_or_else(|| ".".into())),
        "grep" | "find" => field("pattern").map(|pattern| match field("path") {
            Some(path) => format!("{pattern} in {path}"),
            None => pattern,
        }),
        // 0.3 `presentation.ts`: a subagent by its label and route, a
        // message by its first line.
        "subagent" => field("description").map(|label| {
            let label = clip(first_line(&label), 80);
            match (field("provider"), field("model")) {
                (Some(provider), Some(model)) => format!("{label} · {provider}/{model}"),
                _ => label,
            }
        }),
        "send_message" => Some(clip(first_line(&field("message").unwrap_or_default()), 48)),
        // 0.3 `tool-web`: a fetch by its URL, a search by its queries.
        "web_fetch" => field("url"),
        "web_search" => args
            .get("queries")
            .and_then(Value::as_array)
            .map(|queries| {
                queries
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(", ")
            }),
        "job_output" | "job_kill" | "interrupt_agent" => label(),
        "list_agents" | "list_subagent_models" => Some(field("agent_id").unwrap_or_else(|| {
            [field("provider"), field("model"), field("scope")]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join("/")
        })),
        _ => None,
    };
    lead.unwrap_or_else(|| match args {
        Value::Object(map) if map.is_empty() => String::new(),
        other => other.to_string(),
    })
}

/// Records the label of the background job or subagent a result started.
pub(super) fn learn(labels: &mut Labels, details: Option<&Value>) {
    let Some(details) = details else {
        return;
    };
    for (key, id) in [(BACKGROUND_JOB_DETAIL, "id"), (SUBAGENT_DETAIL, "agentId")] {
        let field = |name| details.get(key)?.get(name)?.as_str();
        if let (Some(id), Some(label)) = (field(id), field("label")) {
            labels.insert(id.to_owned(), label.to_owned());
        }
    }
}

/// A running call's outcome from a partial result: its output so far, or
/// for a `codemode` call the calls its script has made.
pub(super) fn partial(tool: &str, result: &AgentToolResult) -> Outcome {
    if tool == CODEMODE_TOOL_NAME {
        return Outcome {
            calls: nested_calls(result.details.as_ref()),
            ..Outcome::running(Vec::new())
        };
    }
    Outcome::running(result_lines(&result.content, result.details.as_ref()))
}

/// The calls a `codemode` result's `details.calls` records, Pi's
/// `CodemodeNestedCall`s. A record this frontend cannot read is left out.
fn nested_calls(details: Option<&Value>) -> Vec<NestedCall> {
    let Some(calls) = details
        .and_then(|details| details.get("calls"))
        .and_then(Value::as_array)
    else {
        return Vec::new();
    };
    calls
        .iter()
        .filter_map(|call| {
            let text = |key: &str| call.get(key).and_then(Value::as_str).map(str::to_owned);
            let status = match call.get("status")?.as_str()? {
                "running" => NestedStatus::Running,
                "ok" => NestedStatus::Ok,
                "error" => NestedStatus::Error,
                "cancelled" => NestedStatus::Cancelled,
                _ => return None,
            };
            Some(NestedCall {
                name: text("name")?,
                args: text("args").unwrap_or_default(),
                status,
                duration: call
                    .get("durationMs")
                    .and_then(Value::as_f64)
                    .and_then(|ms| Duration::try_from_secs_f64(ms / 1000.0).ok()),
                error: text("error"),
            })
        })
        .collect()
}

/// A settled call's outcome from its result.
pub(super) fn settled(tool: &str, failed: bool, result: &AgentToolResult) -> Outcome {
    outcome(tool, failed, &result.content, result.details.as_ref())
}

/// What a settled call's card shows. A call that started a background job
/// or subagent is tagged with its id and says it went to the background; a
/// call that reached one says what came of it, such as `exit 0` or
/// `delivered`, rather than repeating the text the model reads. A failed
/// call shows its error as it came.
pub(super) fn outcome(
    tool: &str,
    failed: bool,
    content: &[UserContentBlock],
    details: Option<&Value>,
) -> Outcome {
    let mut outcome = Outcome {
        state: if failed {
            CallState::Failed
        } else {
            CallState::Done
        },
        ..Outcome::running(result_lines(content, details))
    };
    let record = |key: &str| details.and_then(|details| details.get(key));
    let started = |key: &str, id: &str| {
        record(key)
            .and_then(|record| record.get(id))
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
            .map(str::to_owned)
    };
    let background = |outcome: &mut Outcome| {
        if !failed {
            outcome.summary = Some(copy::BACKGROUND.to_owned());
            outcome.output.clear();
        }
    };
    match tool {
        // Pi's renderer leaves out the header a script's result opens
        // with; the time it states is the block's.
        CODEMODE_TOOL_NAME => {
            let header = match content.first() {
                Some(UserContentBlock::Text(text)) => codemode::wall_time(&text.text),
                _ => None,
            };
            let rest = if header.is_some() {
                &content[1..]
            } else {
                content
            };
            outcome.output = result_lines(rest, None);
            outcome.calls = nested_calls(details);
            outcome.full_output_path = details
                .and_then(|details| details.get("fullOutputPath"))
                .and_then(Value::as_str)
                .map(str::to_owned);
            outcome.wall_time = header;
        }
        "bash" => {
            outcome.id = started(BACKGROUND_JOB_DETAIL, "id");
            if outcome.id.is_some() {
                background(&mut outcome);
            }
        }
        "subagent" => {
            outcome.id = started(SUBAGENT_DETAIL, "agentId");
            let foreground = record(SUBAGENT_DETAIL)
                .and_then(|record| record.get("foreground"))
                .and_then(Value::as_bool)
                == Some(true);
            if outcome.id.is_some() && !foreground {
                background(&mut outcome);
            }
        }
        _ if failed => {}
        "job_output" => {
            if outcome
                .output
                .last()
                .is_some_and(|line| line == STILL_RUNNING)
            {
                outcome.output.pop();
            }
            if let Some(state) = outcome.output.last().and_then(|line| job_state(line)) {
                outcome.output.pop();
                outcome.summary = Some(jobs::word(&state));
            }
            if outcome.output == ["(no new output)"] {
                outcome.output.clear();
            }
        }
        "job_kill" => {
            let text = outcome.output.join("\n");
            let finished = text
                .rfind("[status: ")
                .and_then(|at| job_state(&text[at..]));
            outcome.summary = Some(match finished {
                Some(state) => format!(
                    "{}{}{}",
                    copy::ALREADY_FINISHED,
                    copy::STATUS_SEP,
                    jobs::word(&state)
                ),
                None => copy::JOB_STOPPING.to_owned(),
            });
            outcome.output.clear();
        }
        "interrupt_agent" | "send_message" => {
            let word = if tool == "send_message" {
                copy::DELIVERED
            } else {
                copy::JOB_STOPPING
            };
            outcome.summary = Some(word.to_owned());
            outcome.output.clear();
        }
        // An empty listing, `(no background jobs)`, says only that.
        "job_list" | "list_agents" => {
            if let [only] = outcome.output.as_slice()
                && only.starts_with('(')
                && only.ends_with(')')
            {
                outcome.summary = Some(copy::NONE.to_owned());
                outcome.output.clear();
            }
        }
        _ => {}
    }
    outcome
}

/// The job state a `[status: …]` line names, such as `[status: completed,
/// exit code: 0]`.
pub(super) fn job_state(line: &str) -> Option<JobState> {
    let inner = line.strip_prefix("[status: ")?.strip_suffix(']')?;
    let (status, detail) = match inner.split_once(", ") {
        Some((status, detail)) => (status, Some(detail.to_owned())),
        None => (inner, None),
    };
    Some(match status {
        "running" => JobState::Running,
        "stopping" => JobState::Stopping,
        "completed" => JobState::Completed(detail),
        "killed" => JobState::Killed(detail),
        "failed" => JobState::Failed(detail),
        _ => return None,
    })
}

/// `text`'s first nonblank line with runs of whitespace collapsed, and
/// ` …` when more lines follow.
fn first_line(text: &str) -> String {
    let mut lines = text
        .lines()
        .map(|line| line.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|line| !line.is_empty());
    let first = lines.next().unwrap_or_default();
    if lines.next().is_some() {
        format!("{first} …")
    } else {
        first
    }
}

/// `text` kept to `limit` characters, the last one an ellipsis when cut.
fn clip(text: String, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text;
    }
    let kept: String = text.chars().take(limit.saturating_sub(1)).collect();
    format!("{}…", kept.trim_end())
}

/// A result's lines: its text, an image as a note, and an edit's unified
/// patch, which the card draws as a diff.
fn result_lines(content: &[UserContentBlock], details: Option<&Value>) -> Vec<String> {
    if let Some(patch) = details
        .and_then(|details| details.get("patch"))
        .and_then(Value::as_str)
    {
        // The hunks only: the card numbers lines from their headers.
        let lines: Vec<String> = patch
            .lines()
            .skip_while(|line| !line.starts_with("@@"))
            .map(str::to_owned)
            .collect();
        if !lines.is_empty() {
            return lines;
        }
    }
    let mut lines = Vec::new();
    for block in content {
        match block {
            UserContentBlock::Text(text) => {
                lines.extend(text.text.trim_end().lines().map(str::to_owned));
            }
            UserContentBlock::Image(image) => lines.push(format!("[image: {}]", image.mime_type)),
        }
    }
    lines
}
