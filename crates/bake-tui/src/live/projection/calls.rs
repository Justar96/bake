//! A tool call's card: its name, target, and argument as the transcript
//! shows them, and its state, summary, and output from its result.

use bake_tui_view::activity::Tones;
use bake_tui_view::shell_output;

use super::*;

/// A tool call's card: its title, its main argument, the job or agent it
/// reached, and its outcome. A running `job_output` that waits says so.
pub(super) fn call_row(tool: &str, args: &Value, labels: &Labels, outcome: Outcome) -> Row {
    if tool == VISUALIZE_TOOL_NAME {
        return visual_row(args, outcome, Args::Complete);
    }
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
    // A write's card shows what it writes, from its arguments, so the
    // content shows while the call runs; a failed write shows its error.
    let output = if interrupted_drops_output(tool) && outcome.state != CallState::Failed {
        written(args)
    } else {
        outcome.output
    };
    Row::Call {
        tool: display_name(tool),
        argument: argument(tool, args, labels),
        fields: fields(tool, args, labels),
        id: outcome.id.or_else(|| target(tool, args)),
        state: outcome.state,
        summary,
        output,
        full_output_path: outcome.full_output_path,
    }
}

/// Whether a tool's running card shows output drawn from its arguments
/// rather than from a result, so a call cut off before its result shows
/// none, as a replay of its log does: a `write`'s content.
pub(super) fn interrupted_drops_output(tool: &str) -> bool {
    tool == "write"
}

/// The content a `write` call writes, as a diff's added lines.
fn written(args: &Value) -> Vec<String> {
    args.get("content")
        .and_then(Value::as_str)
        .map(|content| content.lines().map(|line| format!("+{line}")).collect())
        .unwrap_or_default()
}

/// A tool's card title: its entry in [`copy::TOOL_TITLES`]; for an MCP
/// tool, its server and its own name; or its name with the first letter
/// capitalized.
pub(super) fn display_name(tool: &str) -> String {
    if let Some((_, title)) = copy::TOOL_TITLES.iter().find(|(name, _)| *name == tool) {
        return (*title).to_owned();
    }
    // `mcp__<server>__<tool>` (`bake_coding_agent::mcp::public_tool_name`)
    // is never parsed back to call the tool; it is only shown here, split
    // at the first `__` after the prefix, so a server named with `__` reads
    // with the rest of its name before the tool's.
    if let Some((server, name)) = tool
        .strip_prefix(MCP_TOOL_PREFIX)
        .and_then(|rest| rest.split_once("__"))
        .filter(|(server, name)| !server.is_empty() && !name.is_empty())
    {
        return format!("{server}{}{name}", copy::MCP_TOOL_SEP);
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
/// subagent, its label, since its id is tagged. Any other tool's arguments
/// are its card's [`fields`], or JSON when they are not an object.
pub(super) fn argument(tool: &str, args: &Value, labels: &Labels) -> String {
    lead(tool, args, labels).unwrap_or_else(|| match args {
        Value::Object(_) => String::new(),
        other => other.to_string(),
    })
}

/// Most characters of a field's value a card shows before `…`.
const FIELD_CHARS: usize = 48;
/// Most characters of a field's key a card shows before `…`.
const KEY_CHARS: usize = 32;

/// The arguments of a tool its card leads with none of, such as an MCP
/// tool's, as `key value` pairs in the order the model gave them: a string
/// unquoted, on one line, and anything else as JSON, each value cut to
/// [`FIELD_CHARS`] characters with `…`. An empty string reads as `""`, so
/// it shows. None when the card has an argument, or the arguments are not
/// an object.
pub(super) fn fields(tool: &str, args: &Value, labels: &Labels) -> Vec<(String, String)> {
    let Value::Object(map) = args else {
        return Vec::new();
    };
    if lead(tool, args, labels).is_some() {
        return Vec::new();
    }
    map.iter()
        .map(|(key, value)| {
            let value = match value {
                Value::String(text) => match first_line(text) {
                    line if line.is_empty() => r#""""#.to_owned(),
                    line => clip(line, FIELD_CHARS),
                },
                other => clip(other.to_string(), FIELD_CHARS),
            };
            (clip(first_line(key), KEY_CHARS), value)
        })
        .collect()
}

/// The lines a `read` asks for, as Pi's read call writes them
/// (`formatReadLineRange`, checked at Pi v0.78.0): `:120-180` for an offset
/// of 120 and a limit of 61, `:120` for an offset alone, `:1-40` for a
/// limit alone, and nothing for neither. A range ending at 0 is left
/// open, as Pi's is.
fn read_range(args: &Value) -> String {
    let number = |key| args.get(key).and_then(Value::as_f64);
    let (offset, limit) = (number("offset"), number("limit"));
    if offset.is_none() && limit.is_none() {
        return String::new();
    }
    let start = offset.unwrap_or(1.0);
    match limit.map(|limit| start + limit - 1.0) {
        Some(end) if end != 0.0 => format!(":{start}-{end}"),
        _ => format!(":{start}"),
    }
}

/// The argument a known tool's card leads with, or `None` for a tool it
/// has none for.
fn lead(tool: &str, args: &Value, labels: &Labels) -> Option<String> {
    let field = |name: &str| args.get(name).and_then(Value::as_str).map(str::to_owned);
    let label = || {
        Some(
            target(tool, args)
                .and_then(|id| labels.get(&id).cloned())
                .unwrap_or_default(),
        )
    };
    match tool {
        "bash" => field("command"),
        "read" => field("path").map(|path| format!("{path}{}", read_range(args))),
        "write" | "edit" => field("path"),
        "ls" => Some(field("path").unwrap_or_else(|| ".".into())),
        // Pi's grep call adds its glob in parentheses.
        "grep" | "find" => field("pattern").map(|pattern| {
            let mut lead = match field("path") {
                Some(path) => format!("{pattern} in {path}"),
                None => pattern,
            };
            if let Some(glob) = field("glob").filter(|glob| !glob.is_empty()) {
                lead.push_str(&format!(" ({glob})"));
            }
            lead
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
    }
}

/// Most characters the phase naming a running call shows, `…` included.
const ACTION_CHARS: usize = 40;

/// What a running call does, as the turn's phase names it and its card
/// reads, lowercase after the bar's activity word: `reading transcript.rs`,
/// `running cargo test`, `searching ctx.effect`, `fetching docs.rs`; a tool
/// its card leads with no argument for by its title, such as `job output`,
/// and an MCP tool as its card names it, `github · create_issue`. One
/// short phrase on one line, cut with `…` past [`ACTION_CHARS`]
/// characters, with no escape sequence or control character.
pub(super) fn action(tool: &str, args: &Value, labels: &Labels) -> String {
    let field = |name: &str| {
        args.get(name)
            .and_then(Value::as_str)
            .map(first_line)
            .filter(|text| !text.is_empty())
    };
    // A file by its name: its card dims the directory.
    let file = |path: String| {
        path.trim_end_matches('/')
            .rsplit('/')
            .next()
            .filter(|name| !name.is_empty())
            .map_or(path.clone(), str::to_owned)
    };
    let doing =
        |verb: &str, object: Option<String>| object.map(|object| format!("{verb} {object}"));
    let phrase = match tool {
        "read" => doing(copy::PHASE_READING, field("path").map(file)),
        "write" => doing(copy::PHASE_WRITING, field("path").map(file)),
        "edit" => doing(copy::PHASE_EDITING, field("path").map(file)),
        "bash" => doing(
            copy::PHASE_RUNNING,
            field("command").and_then(|c| program(&c)),
        ),
        "grep" => doing(copy::PHASE_SEARCHING, field("pattern")),
        "find" => doing(copy::PHASE_FINDING, field("pattern")),
        "ls" => doing(
            copy::PHASE_LISTING,
            Some(field("path").unwrap_or_else(|| ".".into())),
        ),
        "web_fetch" => doing(
            copy::PHASE_FETCHING,
            field("url").map(|url| cards::host(&url).map_or(url.clone(), str::to_owned)),
        ),
        "web_search" => Some(match lead(tool, args, labels).filter(|q| !q.is_empty()) {
            Some(queries) => format!(
                "{} {} {}",
                copy::PHASE_SEARCHING_WEB,
                copy::PHASE_FOR,
                first_line(&queries)
            ),
            None => copy::PHASE_SEARCHING_WEB.to_owned(),
        }),
        _ => None,
    };
    // Any other call by its card's title, lowercase like the verbs, except
    // an MCP call, whose `server · tool` keeps the names the server gave.
    let phrase = phrase.unwrap_or_else(|| {
        let title = display_name(tool);
        if tool.starts_with(MCP_TOOL_PREFIX) {
            return title;
        }
        let mut chars = title.chars();
        chars
            .next()
            .map(|first| first.to_lowercase().chain(chars).collect())
            .unwrap_or_default()
    });
    let clean = shell_output::parse_sgr(&phrase, Tones::None).0;
    clip(first_line(&clean), ACTION_CHARS)
}

/// The program a shell command runs, with its subcommand when the word
/// after it is a plain one: `cargo test` for `cargo test --locked`, `rg`
/// for `rg -n x`. Leading variable assignments, such as `RUST_LOG=1`, are
/// passed over, and a program named by its path reads by its name.
fn program(command: &str) -> Option<String> {
    let mut words = command
        .split_whitespace()
        .skip_while(|word| word.contains('=') && !word.starts_with('='));
    let program = words.next()?;
    let program = program
        .rsplit('/')
        .next()
        .filter(|name| !name.is_empty())
        .unwrap_or(program);
    let plain = |word: &&str| {
        word.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
            && word
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == ':')
    };
    Some(match words.next().filter(plain) {
        Some(sub) => format!("{program} {sub}"),
        None => program.to_owned(),
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
    let mut outcome = Outcome::running(result_lines(&result.content, result.details.as_ref()));
    full_output(&mut outcome, result.details.as_ref());
    outcome
}

/// Names the file the result's whole output went to, when the tool cut it,
/// and takes off the `[…]` notice line naming it, and the blank line before
/// it: the card names the file under the output instead.
fn full_output(outcome: &mut Outcome, details: Option<&Value>) {
    let Some(path) = details
        .and_then(|details| details.get("fullOutputPath"))
        .and_then(Value::as_str)
        .filter(|path| !path.is_empty())
    else {
        return;
    };
    outcome.full_output_path = Some(path.to_owned());
    let notice = outcome
        .output
        .iter()
        .rposition(|line| line.starts_with('[') && line.ends_with(']') && line.contains(path));
    if let Some(at) = notice {
        outcome.output.remove(at);
        if at > 0 && outcome.output[at - 1].trim().is_empty() {
            outcome.output.remove(at - 1);
        }
    }
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
    if tool != CODEMODE_TOOL_NAME {
        full_output(&mut outcome, details);
    }
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
        "read" => read(&mut outcome, content, details),
        "grep" => grep(&mut outcome, details),
        "find" | "ls" => listing(tool, &mut outcome, details),
        "web_fetch" => fetch(&mut outcome, content, details),
        "web_search" => search(&mut outcome, details),
        _ => {}
    }
    outcome
}

/// A status from its parts, such as `61 lines · truncated`.
fn status(parts: Vec<String>) -> Option<String> {
    (!parts.is_empty()).then(|| parts.join(copy::STATUS_SEP))
}

/// Whether `details` has `key` set to anything but `false` or `null`, as
/// the tools set their limits: a number, or `true`.
fn flagged(details: Option<&Value>, key: &str) -> bool {
    details
        .and_then(|details| details.get(key))
        .is_some_and(|value| !matches!(value, Value::Null | Value::Bool(false)))
}

/// Takes off the `[…]` notice the `grep`, `find`, and `ls` tools end their
/// output with when they set any detail, such as `[100 matches limit
/// reached. …]`; the card's status says it instead.
fn strip_notice(output: &mut Vec<String>, details: Option<&Value>) {
    if details.is_none_or(|details| details.as_object().is_none_or(|map| map.is_empty())) {
        return;
    }
    if output
        .last()
        .is_some_and(|line| line.starts_with('[') && line.ends_with(']'))
    {
        output.pop();
        if output.last().is_some_and(|line| line.trim().is_empty()) {
            output.pop();
        }
    }
}

/// The word a `read`'s image notes use when the image never reached the
/// model, as in `[Image omitted: …]` and the note for a model that takes no
/// images.
const IMAGE_OMITTED: &str = "omitted";

/// A `read`'s card says how many lines it read, and whether they were cut,
/// and keeps the file's text for the expanded card, which numbers it from
/// the line the read started at: the preview draws none of it, since the
/// model's copy is the result. The read's own notes, such as where to
/// continue, are left out. An image says its type, and that it was left out
/// of the request when it was; a line too long to read says so.
fn read(outcome: &mut Outcome, content: &[UserContentBlock], details: Option<&Value>) {
    let image = content.iter().find_map(|block| match block {
        UserContentBlock::Image(image) => Some(image.mime_type.clone()),
        UserContentBlock::Text(_) => None,
    });
    let text = content
        .iter()
        .find_map(|block| match block {
            UserContentBlock::Text(text) => Some(text.text.as_str()),
            UserContentBlock::Image(_) => None,
        })
        .unwrap_or_default();
    // `Read image file [image/png]`, then the image's notes, if any.
    let note = text
        .strip_prefix("Read image file [")
        .and_then(|rest| rest.split_once(']'));
    if image.is_some() || note.is_some() {
        let omitted =
            image.is_none() || note.is_some_and(|(_, notes)| notes.contains(IMAGE_OMITTED));
        let mime = image.or_else(|| note.map(|(mime, _)| mime.to_owned()));
        let parts = mime
            .filter(|mime| !mime.is_empty())
            .into_iter()
            .chain(omitted.then(|| copy::OMITTED.to_owned()))
            .collect();
        outcome.summary = status(parts);
        outcome.output.clear();
        return;
    }
    let truncation = details.and_then(|details| details.get("truncation"));
    let field = |key: &str| truncation.and_then(|truncation| truncation.get(key));
    if field("firstLineExceedsLimit").and_then(Value::as_bool) == Some(true) {
        outcome.summary = Some(copy::LINE_TOO_LONG.to_owned());
        outcome.output.clear();
        return;
    }
    // The file's text, without the `[… to continue.]` note a cut or
    // limited read ends with.
    let file = text
        .rsplit_once("\n\n")
        .filter(|(_, note)| note.starts_with('[') && note.ends_with("to continue.]"))
        .map_or(text, |(file, _)| file);
    let lines = if field("truncated").and_then(Value::as_bool) == Some(true) {
        field("outputLines").and_then(Value::as_u64).map(|lines| {
            [
                cards::count(
                    usize::try_from(lines).unwrap_or(usize::MAX),
                    copy::LINE_NOUNS,
                ),
                copy::TRUNCATED.to_owned(),
            ]
            .join(copy::STATUS_SEP)
        })
    } else {
        Some(match file.lines().count() {
            0 => copy::EMPTY.to_owned(),
            lines => cards::count(lines, copy::LINE_NOUNS),
        })
    };
    if lines.is_some() {
        outcome.summary = lines;
        outcome.output = file.lines().map(str::to_owned).collect();
    }
}

/// A `grep`'s card counts its matches and the files they are in, says
/// whether it reached its limit, and keeps its lines for the card to group.
fn grep(outcome: &mut Outcome, details: Option<&Value>) {
    if outcome.output == ["No matches found"] {
        outcome.summary = Some(copy::NO_MATCHES.to_owned());
        outcome.output.clear();
        return;
    }
    strip_notice(&mut outcome.output, details);
    let mut parts = Vec::new();
    if let Some((matches, files)) = cards::grep_counts(&outcome.output) {
        parts.push(cards::count(matches, copy::MATCH_NOUNS));
        parts.push(cards::count(files, copy::FILE_NOUNS));
    }
    if flagged(details, "matchLimitReached") {
        parts.push(copy::LIMIT_REACHED.to_owned());
    }
    if flagged(details, "truncation") || flagged(details, "linesTruncated") {
        parts.push(copy::TRUNCATED.to_owned());
    }
    outcome.summary = status(parts);
}

/// A `find`'s or `ls`'s card counts what it found, a `find`'s files apart
/// from its directories, and says whether it reached its limit.
fn listing(tool: &str, outcome: &mut Outcome, details: Option<&Value>) {
    let none = match tool {
        "find" => ("No files found matching pattern", copy::NO_PATHS_FOUND),
        _ => ("(empty directory)", copy::EMPTY),
    };
    if outcome.output == [none.0] {
        outcome.summary = Some(none.1.to_owned());
        outcome.output.clear();
        return;
    }
    strip_notice(&mut outcome.output, details);
    let entries: Vec<&String> = outcome
        .output
        .iter()
        .filter(|line| !line.trim().is_empty())
        .collect();
    let mut parts = Vec::new();
    if tool == "find" {
        let directories = entries.iter().filter(|entry| entry.ends_with('/')).count();
        let files = entries.len() - directories;
        if files > 0 || directories == 0 {
            parts.push(cards::count(files, copy::FILE_NOUNS));
        }
        if directories > 0 {
            parts.push(cards::count(directories, copy::DIRECTORY_NOUNS));
        }
    } else {
        parts.push(cards::count(entries.len(), copy::ENTRY_NOUNS));
    }
    if flagged(details, "resultLimitReached") || flagged(details, "entryLimitReached") {
        parts.push(copy::LIMIT_REACHED.to_owned());
    }
    if flagged(details, "truncation") {
        parts.push(copy::TRUNCATED.to_owned());
    }
    outcome.summary = status(parts);
}

/// A `web_fetch`'s card says the page's HTTP status and the size of the
/// text the model read, and keeps that text for the expanded card: the
/// preview draws none of it, since the result is the model's.
fn fetch(outcome: &mut Outcome, content: &[UserContentBlock], details: Option<&Value>) {
    let Some(code) = details
        .and_then(|details| details.get("statusCode"))
        .and_then(Value::as_u64)
    else {
        return;
    };
    let text: String = content
        .iter()
        .filter_map(|block| match block {
            UserContentBlock::Text(text) => Some(text.text.as_str()),
            UserContentBlock::Image(_) => None,
        })
        .collect();
    // The page after the `Fetched … (HTTP …)` header and the notice.
    let page = text
        .split_once(EXTERNAL_WEB_CONTENT_NOTICE)
        .map_or(text.as_str(), |(_, page)| page);
    let mut parts = vec![format!("HTTP {code}"), cards::size(page.trim().len())];
    if flagged(details, "truncated") {
        parts.push(copy::TRUNCATED.to_owned());
    }
    outcome.summary = status(parts);
    outcome.output = page.trim().lines().map(str::to_owned).collect();
}

/// A `web_search`'s card lists its sources, each as its title and its
/// address on one line for the card to draw, counts them, and says whether
/// more were left out. A search with no sources shows the provider's
/// answer, if any.
fn search(outcome: &mut Outcome, details: Option<&Value>) {
    let Some(sources) = details
        .and_then(|details| details.get("sources"))
        .and_then(Value::as_array)
    else {
        return;
    };
    let text = |source: &Value, key: &str| {
        source
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned()
    };
    outcome.output = sources
        .iter()
        .map(|source| {
            let url: String = text(source, "url").split_whitespace().collect();
            format!("{} {url}", first_line(&text(source, "title")))
        })
        .collect();
    let mut parts = Vec::new();
    if sources.is_empty() {
        let answer = details
            .and_then(|details| details.get("answer"))
            .and_then(Value::as_str)
            .unwrap_or_default();
        outcome.output = answer.trim().lines().map(str::to_owned).collect();
        if outcome.output.is_empty() {
            parts.push(copy::NO_RESULTS.to_owned());
        }
    } else {
        parts.push(cards::count(sources.len(), copy::RESULT_NOUNS));
    }
    if flagged(details, "truncated") {
        parts.push(copy::LIMIT_REACHED.to_owned());
    }
    outcome.summary = status(parts);
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
