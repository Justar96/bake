//! The ConPTY host's line protocol, pure so it is tested on every platform.
//!
//! Every message is one JSON object on one line, in UTF-8. Lines may end in
//! LF or CRLF. Bytes for or from the pseudoconsole travel as standard
//! base64, so the driver sees exactly what conhost wrote, split anywhere.
//!
//! The driver writes commands to the host's standard input:
//!
//! | `op` | Fields | Effect |
//! | --- | --- | --- |
//! | `write` | `data` | Writes the bytes to the pseudoconsole's input |
//! | `resize` | `cols`, `rows` | `ResizePseudoConsole` |
//! | `modes` | `id` | The client reads `GetConsoleMode` of `CONIN$` and `CONOUT$` |
//! | `ctrl` | `event`: `c` or `break` | The client calls `GenerateConsoleCtrlEvent` |
//! | `kill` | | `TerminateProcess` on the program, exit code 1 |
//! | `suspend`, `resume` | `id` | Suspends or resumes every thread of the program |
//! | `members` | `id` | Pins the job's current members and lists every pin |
//! | `unpin` | | Closes the handles of pinned members |
//! | `hangup` | | `ClosePseudoConsole` while clients may still run |
//! | `close` | | Terminates the job, closes the pseudoconsole, and ends |
//!
//! The host writes events to its standard output, each with an `event`
//! field: `output` (`data`), `modes` (`phase`: `initial`, `running`, or
//! `final`; `input`, `output`, and the request's `id` when one asked),
//! `spawned` (`pid`), `exit` (`code`, the program's), `client-exit`
//! (`code`), `suspended` and `resumed` (`id`, `threads`), `members` (`id`,
//! `members`: `pid`, `image`, `role`, `alive`), `eof` (the output pipe
//! ended), `settled` (`terminated`: whether the job had to be terminated),
//! `error` (`message`), and last `closed`.
//!
//! The host and its client speak the same protocol over a pair of pipes:
//! the host sends `modes`, `ctrl`, and `release`; the client sends
//! `modes`, `spawned`, and `error`.
#![cfg_attr(
    not(windows),
    allow(dead_code, reason = "only the Windows host runs it; tested everywhere")
)]

use std::ffi::OsString;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde_json::{Map, Value, json};

/// The longest error text an event carries, in characters.
pub const MAX_MESSAGE: usize = 512;

/// A console control event the client generates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ctrl {
    /// `CTRL_C_EVENT`.
    C,
    /// `CTRL_BREAK_EVENT`.
    Break,
}

/// A command from the driver, or from the host to its client.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    Write(Vec<u8>),
    Resize {
        cols: i16,
        rows: i16,
    },
    Modes {
        id: u64,
    },
    Ctrl(Ctrl),
    Kill,
    Suspend {
        id: u64,
    },
    Resume {
        id: u64,
    },
    Members {
        id: u64,
    },
    Unpin,
    Hangup,
    Close,
    /// Host to client: the program is pinned; the client may exit.
    Release,
}

/// Cuts `text` to [`MAX_MESSAGE`] characters, marking the cut.
pub fn bounded(text: &str) -> String {
    if text.chars().count() <= MAX_MESSAGE {
        return text.to_owned();
    }
    let mut cut: String = text.chars().take(MAX_MESSAGE).collect();
    cut.push('…');
    cut
}

fn field<'a>(object: &'a Map<String, Value>, name: &str) -> Result<&'a Value, String> {
    object
        .get(name)
        .ok_or_else(|| format!("missing field `{name}`"))
}

fn id(object: &Map<String, Value>) -> Result<u64, String> {
    field(object, "id")?
        .as_u64()
        .ok_or_else(|| "`id` is not a non-negative integer".to_owned())
}

fn size(object: &Map<String, Value>, name: &str) -> Result<i16, String> {
    let value = field(object, name)?
        .as_u64()
        .ok_or_else(|| format!("`{name}` is not a non-negative integer"))?;
    match i16::try_from(value) {
        Ok(cells) if cells > 0 => Ok(cells),
        _ => Err(format!("`{name}` must be between 1 and {}", i16::MAX)),
    }
}

/// Parses one command line. The error is bounded and names the problem.
pub fn parse_command(line: &str) -> Result<Command, String> {
    let line = line.strip_suffix('\n').unwrap_or(line);
    let line = line.strip_suffix('\r').unwrap_or(line);
    let value: Value =
        serde_json::from_str(line).map_err(|error| bounded(&format!("not JSON: {error}")))?;
    let Value::Object(object) = value else {
        return Err("a command is not a JSON object".to_owned());
    };
    let op = field(&object, "op")?
        .as_str()
        .ok_or_else(|| "`op` is not a string".to_owned())?;
    let command = match op {
        "write" => {
            let data = field(&object, "data")?
                .as_str()
                .ok_or_else(|| "`data` is not a string".to_owned())?;
            Command::Write(
                STANDARD
                    .decode(data)
                    .map_err(|error| bounded(&format!("`data` is not base64: {error}")))?,
            )
        }
        "resize" => Command::Resize {
            cols: size(&object, "cols")?,
            rows: size(&object, "rows")?,
        },
        "modes" => Command::Modes { id: id(&object)? },
        "ctrl" => match field(&object, "event")?.as_str() {
            Some("c") => Command::Ctrl(Ctrl::C),
            Some("break") => Command::Ctrl(Ctrl::Break),
            _ => return Err("`event` must be \"c\" or \"break\"".to_owned()),
        },
        "kill" => Command::Kill,
        "suspend" => Command::Suspend { id: id(&object)? },
        "resume" => Command::Resume { id: id(&object)? },
        "members" => Command::Members { id: id(&object)? },
        "unpin" => Command::Unpin,
        "hangup" => Command::Hangup,
        "close" => Command::Close,
        "release" => Command::Release,
        other => return Err(bounded(&format!("unknown op `{other}`"))),
    };
    Ok(command)
}

/// The line for `command`, as the host sends it to its client.
pub fn command_line(command: &Command) -> String {
    let value = match command {
        Command::Write(data) => json!({ "op": "write", "data": STANDARD.encode(data) }),
        Command::Resize { cols, rows } => json!({ "op": "resize", "cols": cols, "rows": rows }),
        Command::Modes { id } => json!({ "op": "modes", "id": id }),
        Command::Ctrl(Ctrl::C) => json!({ "op": "ctrl", "event": "c" }),
        Command::Ctrl(Ctrl::Break) => json!({ "op": "ctrl", "event": "break" }),
        Command::Kill => json!({ "op": "kill" }),
        Command::Suspend { id } => json!({ "op": "suspend", "id": id }),
        Command::Resume { id } => json!({ "op": "resume", "id": id }),
        Command::Members { id } => json!({ "op": "members", "id": id }),
        Command::Unpin => json!({ "op": "unpin" }),
        Command::Hangup => json!({ "op": "hangup" }),
        Command::Close => json!({ "op": "close" }),
        Command::Release => json!({ "op": "release" }),
    };
    format!("{value}\n")
}

/// An event line: `{"event": kind, ...fields}`.
pub fn event(kind: &str, fields: Value) -> String {
    let mut object = Map::new();
    object.insert("event".to_owned(), Value::from(kind));
    if let Value::Object(fields) = fields {
        object.extend(fields);
    }
    format!("{}\n", Value::Object(object))
}

/// An `output` event for bytes read from the pseudoconsole.
pub fn output(bytes: &[u8]) -> String {
    event("output", json!({ "data": STANDARD.encode(bytes) }))
}

/// An `error` event, its message bounded.
pub fn error(message: &str) -> String {
    event("error", json!({ "message": bounded(message) }))
}

/// A `modes` event; `id` is the request's, when one asked.
pub fn modes(phase: &str, input: u32, output: u32, id: Option<u64>) -> String {
    let mut fields = json!({ "phase": phase, "input": input, "output": output });
    if let (Some(id), Value::Object(object)) = (id, &mut fields) {
        object.insert("id".to_owned(), Value::from(id));
    }
    event("modes", fields)
}

/// Whether a line from the client is an event the host may forward: a JSON
/// object with a string `event` the client is allowed to send.
pub fn client_event(line: &str) -> Option<Map<String, Value>> {
    let line = line.trim_end_matches(['\r', '\n']);
    let Ok(Value::Object(object)) = serde_json::from_str::<Value>(line) else {
        return None;
    };
    matches!(
        object.get("event").and_then(Value::as_str),
        Some("modes" | "spawned" | "error")
    )
    .then_some(object)
}

/// The host's command line: `host --cols N --rows N [--cwd DIR]
/// [--history] -- PROGRAM ARGS...`.
#[derive(Debug, PartialEq, Eq)]
pub struct HostArgs {
    pub cols: i16,
    pub rows: i16,
    pub cwd: Option<OsString>,
    pub history: bool,
    pub program: Vec<OsString>,
}

/// The client's command line: `client --control-in PIPE --control-out PIPE
/// [--history] -- PROGRAM ARGS...`, naming the host's two control pipes.
#[derive(Debug, PartialEq, Eq)]
pub struct ClientArgs {
    pub control_in: OsString,
    pub control_out: OsString,
    pub history: bool,
    pub program: Vec<OsString>,
}

fn take_value(rest: &mut std::slice::Iter<'_, OsString>, name: &str) -> Result<OsString, String> {
    rest.next()
        .cloned()
        .ok_or_else(|| format!("{name} needs a value"))
}

fn number<T: std::str::FromStr>(value: &OsString, name: &str) -> Result<T, String> {
    value
        .to_str()
        .and_then(|text| text.parse().ok())
        .ok_or_else(|| format!("{name} is not a number"))
}

/// Parses the arguments after `host`.
pub fn host_args(args: &[OsString]) -> Result<HostArgs, String> {
    let (mut cols, mut rows, mut cwd, mut history) = (None, None, None, false);
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        match arg.to_str() {
            Some("--cols") => {
                cols = Some(number::<u16>(&take_value(&mut rest, "--cols")?, "--cols")?)
            }
            Some("--rows") => {
                rows = Some(number::<u16>(&take_value(&mut rest, "--rows")?, "--rows")?)
            }
            Some("--cwd") => cwd = Some(take_value(&mut rest, "--cwd")?),
            Some("--history") => history = true,
            Some("--") => break,
            _ => {
                return Err(bounded(&format!(
                    "unknown argument {}",
                    arg.to_string_lossy()
                )));
            }
        }
    }
    let program: Vec<OsString> = rest.cloned().collect();
    if program.is_empty() {
        return Err("no program after --".to_owned());
    }
    let cells = |value: Option<u16>, name: &str| match value.map(i16::try_from) {
        Some(Ok(cells)) if cells > 0 => Ok(cells),
        Some(_) => Err(format!("{name} must be between 1 and {}", i16::MAX)),
        None => Err(format!("{name} is required")),
    };
    Ok(HostArgs {
        cols: cells(cols, "--cols")?,
        rows: cells(rows, "--rows")?,
        cwd,
        history,
        program,
    })
}

/// Parses the arguments after `client`.
pub fn client_args(args: &[OsString]) -> Result<ClientArgs, String> {
    let (mut control_in, mut control_out, mut history) = (None, None, false);
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        match arg.to_str() {
            Some("--control-in") => control_in = Some(take_value(&mut rest, "--control-in")?),
            Some("--control-out") => control_out = Some(take_value(&mut rest, "--control-out")?),
            Some("--history") => history = true,
            Some("--") => break,
            _ => {
                return Err(bounded(&format!(
                    "unknown argument {}",
                    arg.to_string_lossy()
                )));
            }
        }
    }
    let program: Vec<OsString> = rest.cloned().collect();
    if program.is_empty() {
        return Err("no program after --".to_owned());
    }
    Ok(ClientArgs {
        control_in: control_in.ok_or("--control-in is required")?,
        control_out: control_out.ok_or("--control-out is required")?,
        history,
        program,
    })
}

/// What the client prints before it starts an inline program, as the POSIX
/// driver's shell does: forty numbered rows of earlier shell output.
pub fn shell_history() -> String {
    (0..40)
        .map(|row| format!("SHELL_HISTORY_{row:02}\r\n"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn os(args: &[&str]) -> Vec<OsString> {
        args.iter().map(OsString::from).collect()
    }

    #[test]
    fn parses_every_command_and_ignores_a_crlf_ending() {
        let cases = [
            (
                r#"{"op":"write","data":"G1tB"}"#,
                Command::Write(b"\x1b[A".to_vec()),
            ),
            (
                r#"{"op":"resize","cols":80,"rows":24}"#,
                Command::Resize { cols: 80, rows: 24 },
            ),
            (r#"{"op":"modes","id":7}"#, Command::Modes { id: 7 }),
            (r#"{"op":"ctrl","event":"c"}"#, Command::Ctrl(Ctrl::C)),
            (
                r#"{"op":"ctrl","event":"break"}"#,
                Command::Ctrl(Ctrl::Break),
            ),
            (r#"{"op":"kill"}"#, Command::Kill),
            (r#"{"op":"suspend","id":1}"#, Command::Suspend { id: 1 }),
            (r#"{"op":"resume","id":2}"#, Command::Resume { id: 2 }),
            (r#"{"op":"members","id":3}"#, Command::Members { id: 3 }),
            (r#"{"op":"unpin"}"#, Command::Unpin),
            (r#"{"op":"hangup"}"#, Command::Hangup),
            (r#"{"op":"close"}"#, Command::Close),
            (r#"{"op":"release"}"#, Command::Release),
        ];
        for (line, expected) in cases {
            assert_eq!(parse_command(line), Ok(expected.clone()), "{line}");
            assert_eq!(
                parse_command(&format!("{line}\r\n")),
                Ok(expected.clone()),
                "{line} CRLF"
            );
            // What the host sends its client reads back as the same command.
            assert_eq!(
                parse_command(&command_line(&expected)),
                Ok(expected),
                "{line} round trip"
            );
        }
    }

    #[test]
    fn refuses_malformed_commands_with_a_bounded_reason() {
        for (line, reason) in [
            ("not json", "not JSON"),
            ("[1]", "not a JSON object"),
            (r#"{"data":"x"}"#, "missing field `op`"),
            (r#"{"op":"write"}"#, "missing field `data`"),
            (r#"{"op":"write","data":"!!"}"#, "not base64"),
            (
                r#"{"op":"resize","cols":0,"rows":24}"#,
                "`cols` must be between 1",
            ),
            (
                r#"{"op":"resize","cols":80,"rows":40000}"#,
                "`rows` must be between 1",
            ),
            (
                r#"{"op":"resize","cols":-1,"rows":24}"#,
                "`cols` is not a non-negative integer",
            ),
            (r#"{"op":"modes"}"#, "missing field `id`"),
            (r#"{"op":"ctrl","event":"close"}"#, "`event` must be"),
            (r#"{"op":"exec"}"#, "unknown op `exec`"),
        ] {
            let error = parse_command(line).expect_err(line);
            assert!(error.contains(reason), "{line}: {error}");
            assert!(error.chars().count() <= MAX_MESSAGE + 1);
        }
        let long = format!(r#"{{"op":"{}"}}"#, "x".repeat(4096));
        let error = parse_command(&long).expect_err("long op");
        assert_eq!(error.chars().count(), MAX_MESSAGE + 1);
        assert!(error.ends_with('…'));
    }

    #[test]
    fn events_are_single_json_lines_carrying_bytes_as_base64() {
        let bytes = "\x1b[?2004h漢\r\n".as_bytes();
        let line = output(bytes);
        assert!(line.ends_with('\n') && !line[..line.len() - 1].contains('\n'));
        let value: Value = serde_json::from_str(&line).expect("JSON");
        assert_eq!(value["event"], "output");
        assert_eq!(
            STANDARD
                .decode(value["data"].as_str().expect("data"))
                .expect("base64"),
            bytes
        );
        let value: Value = serde_json::from_str(&modes("final", 0x1f7, 7, Some(4))).expect("JSON");
        assert_eq!(
            value,
            json!({ "event": "modes", "phase": "final", "input": 0x1f7, "output": 7, "id": 4 })
        );
        let value: Value = serde_json::from_str(&modes("initial", 1, 2, None)).expect("JSON");
        assert!(value.get("id").is_none());
        let value: Value = serde_json::from_str(&error(&"e".repeat(2000))).expect("JSON");
        assert_eq!(
            value["message"].as_str().expect("message").chars().count(),
            MAX_MESSAGE + 1
        );
    }

    #[test]
    fn forwards_only_the_events_a_client_may_send() {
        assert!(client_event(&modes("running", 1, 2, Some(1))).is_some());
        assert!(client_event("{\"event\":\"spawned\",\"pid\":4}\r\n").is_some());
        assert!(client_event(&error("x")).is_some());
        for line in [
            r#"{"event":"exit","code":0}"#,
            r#"{"event":"closed"}"#,
            r#"{"op":"modes","id":1}"#,
            "garbage",
        ] {
            assert!(client_event(line).is_none(), "{line}");
        }
    }

    #[test]
    fn parses_host_and_client_arguments_and_keeps_the_program_whole() {
        let parsed = host_args(&os(&[
            "--cols",
            "80",
            "--rows",
            "24",
            "--cwd",
            r"C:\tmp\ä b",
            "--history",
            "--",
            r"C:\Program Files\bake-rs.exe",
            "--inline",
            "--",
            "a \"b\"",
        ]))
        .expect("host");
        assert_eq!(
            parsed,
            HostArgs {
                cols: 80,
                rows: 24,
                cwd: Some(r"C:\tmp\ä b".into()),
                history: true,
                program: os(&[r"C:\Program Files\bake-rs.exe", "--inline", "--", "a \"b\""]),
            }
        );
        for (args, reason) in [
            (&["--rows", "24", "--", "p"][..], "--cols is required"),
            (
                &["--cols", "0", "--rows", "24", "--", "p"][..],
                "--cols must be between",
            ),
            (
                &["--cols", "x", "--rows", "24", "--", "p"][..],
                "--cols is not a number",
            ),
            (&["--cols", "80", "--rows", "24"][..], "no program"),
            (&["--cols"][..], "--cols needs a value"),
            (&["--bogus"][..], "unknown argument --bogus"),
        ] {
            assert!(
                host_args(&os(args)).expect_err(reason).contains(reason),
                "{args:?}"
            );
        }
        let client = client_args(&os(&[
            "--control-in",
            r"\\.\pipe\in",
            "--control-out",
            r"\\.\pipe\out",
            "--",
            "p",
            "x",
        ]))
        .expect("client");
        assert_eq!(
            client,
            ClientArgs {
                control_in: r"\\.\pipe\in".into(),
                control_out: r"\\.\pipe\out".into(),
                history: false,
                program: os(&["p", "x"])
            }
        );
        assert!(client_args(&os(&["--control-in", "x", "--", "p"])).is_err());
    }

    #[test]
    fn shell_history_has_forty_crlf_rows() {
        let history = shell_history();
        assert_eq!(history.matches("\r\n").count(), 40);
        assert!(
            history.starts_with("SHELL_HISTORY_00\r\n")
                && history.ends_with("SHELL_HISTORY_39\r\n")
        );
    }
}
