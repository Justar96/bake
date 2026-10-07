//! The code-mode script a sample turn runs, played on the preview's clock.
//!
//! Nothing runs: the script's calls follow a fixed timeline, so the same
//! elapsed time always draws the same row. It shows how a program's calls
//! reach the screen as the harness dispatches them: one `glob`, then a read
//! of every manifest under `Promise.all`, at most [`POOL`] at a time as the
//! dispatch pool allows, then a build that runs until the turn ends. What
//! the program logs and returns appears only when it settles, because the
//! runtime hands both back together.

use std::time::Duration;

use crate::copy;
use crate::transcript::{CallState, Nested, Row};

/// Calls a program may have running at once; the PTC `maxParallelSubCalls`
/// default.
pub const POOL: usize = 10;
/// Manifests the sample's `glob` finds.
const MANIFESTS: usize = 12;
/// The manifest the program may not read; it catches the error and goes on.
const DENIED: usize = 7;
const GLOB: Duration = Duration::from_millis(400);

const PACKAGES: [&str; MANIFESTS] = [
    "agent-loop",
    "app-boot",
    "llm",
    "tools",
    "shell",
    "fs",
    "jobs",
    "goal",
    "subagent",
    "skill",
    "web",
    "mcp",
];

/// The program the sample turn runs.
const SOURCE: &[&str] = &[
    "const paths = await tools.glob({ pattern: \"packages/*/package.json\" });",
    "const texts = await Promise.all(",
    "  paths.map((path) => tools.read({ path }).catch(() => \"\")),",
    ");",
    "console.log(`read ${texts.filter(Boolean).length} of ${paths.length}`);",
    "const { exitCode } = await tools.bash({ command: \"bun run build\" });",
    "return { manifests: paths.length, exitCode };",
];

/// How a turn's script is settled: run to its end, or stopped by Esc.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Settle {
    Completed,
    Interrupted,
}

/// When each call starts and, but for the build, ends: the `glob`, the
/// reads in dispatch order, then the build. A read waits for the earliest
/// slot of the pool to free, as the dispatch queue starts calls strictly in
/// submission order.
fn timeline() -> Vec<(Duration, Option<Duration>)> {
    let mut calls = vec![(Duration::ZERO, Some(GLOB))];
    let mut free = [GLOB; POOL];
    let mut last = GLOB;
    for index in 0..MANIFESTS {
        let slot = (0..POOL).min_by_key(|&s| free[s]).unwrap_or(0);
        let start = free[slot];
        let length = Duration::from_millis(300 + (index as u64 * 5 % 7) * 140);
        free[slot] = start + length;
        last = last.max(start + length);
        calls.push((start, Some(start + length)));
    }
    // `Promise.all` settles once every read has, and the build starts then.
    calls.push((last, None));
    calls
}

fn call(index: usize) -> (&'static str, String) {
    match index {
        0 => ("glob", "packages/*/package.json".into()),
        i if i <= MANIFESTS => ("read", format!("packages/{}/package.json", PACKAGES[i - 1])),
        _ => ("bash", copy::SAMPLE_COMMAND.into()),
    }
}

/// What a finished call notes: its size, its error, or the build's exit.
fn note(index: usize) -> (CallState, String) {
    match index {
        0 => (CallState::Done, format!("{MANIFESTS} files")),
        i if i == DENIED + 1 => (CallState::Failed, "Permission denied".into()),
        i if i <= MANIFESTS => (CallState::Done, format!("{} lines", 18 + i * 3 % 11)),
        _ => (CallState::Done, copy::SAMPLE_DONE.into()),
    }
}

fn nested(index: usize, state: CallState, note: Option<String>) -> Nested {
    let (tool, argument) = call(index);
    Nested {
        tool: tool.into(),
        argument,
        state,
        note,
    }
}

fn script(
    state: CallState,
    summary: Option<String>,
    calls: Vec<Nested>,
    logs: Vec<String>,
    result: Option<String>,
) -> Row {
    Row::Script {
        description: copy::SAMPLE_SCRIPT.into(),
        source: SOURCE.iter().map(|line| (*line).to_owned()).collect(),
        state,
        summary,
        calls,
        logs,
        result,
    }
}

fn log() -> String {
    format!("read {} of {MANIFESTS}", MANIFESTS - 1)
}

/// The script as it stands `elapsed` after the turn started: every call
/// dispatched so far, those still running blinking.
pub fn at(elapsed: Duration) -> Row {
    let calls = timeline()
        .into_iter()
        .enumerate()
        .take_while(|(_, (start, _))| *start <= elapsed)
        .map(|(index, (_, end))| match end {
            Some(end) if end <= elapsed => {
                let (state, note) = note(index);
                nested(index, state, Some(note))
            }
            _ => nested(index, CallState::Running, None),
        })
        .collect();
    script(CallState::Running, None, calls, Vec::new(), None)
}

/// Time from `elapsed` until the script next changes, or `None` once only
/// the build is left, which runs until the turn ends.
pub fn next(elapsed: Duration) -> Option<Duration> {
    timeline()
        .into_iter()
        .flat_map(|(start, end)| [Some(start), end])
        .flatten()
        .filter(|&t| t > elapsed)
        .min()
        .map(|t| t - elapsed)
}

/// The script once the turn ends `elapsed` after it started. Completed, it
/// runs to its end: every call settles, and what it logged and returned
/// appear. Interrupted, the calls it had started fail, the ones it had not
/// never run, and it keeps any line it logged before it stopped.
pub fn settle(elapsed: Duration, how: Settle) -> Row {
    let timeline = timeline();
    match how {
        Settle::Completed => {
            let calls = (0..timeline.len())
                .map(|index| {
                    let (state, note) = note(index);
                    nested(index, state, Some(note))
                })
                .collect();
            script(
                CallState::Done,
                None,
                calls,
                vec![log()],
                Some(format!("{{ manifests: {MANIFESTS}, exitCode: 0 }}")),
            )
        }
        Settle::Interrupted => {
            let Row::Script { calls, .. } = at(elapsed) else {
                unreachable!("at() draws a script")
            };
            let built = calls.len() == timeline.len();
            let calls = calls
                .into_iter()
                .map(|call| match call.state {
                    CallState::Running => Nested {
                        state: CallState::Failed,
                        note: Some(copy::INTERRUPTED_NOTE.into()),
                        ..call
                    },
                    _ => call,
                })
                .collect();
            script(
                CallState::Failed,
                Some(copy::INTERRUPTED_NOTE.into()),
                calls,
                if built { vec![log()] } else { Vec::new() },
                None,
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::activity::Tones;
    use crate::transcript::{Look, present};

    fn calls(row: &Row) -> Vec<(String, CallState)> {
        let Row::Script { calls, .. } = row else {
            panic!("{row:?}")
        };
        calls.iter().map(|c| (c.tool.clone(), c.state)).collect()
    }

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn the_glob_runs_first_then_reads_fill_the_pool() {
        assert_eq!(calls(&at(ms(0))), [("glob".into(), CallState::Running)]);
        let reading = calls(&at(GLOB));
        assert_eq!(reading.len(), 1 + POOL);
        assert_eq!(reading[0].1, CallState::Done);
        assert!(
            reading[1..]
                .iter()
                .all(|(t, s)| t == "read" && *s == CallState::Running)
        );
    }

    #[test]
    fn the_build_starts_once_every_read_has_settled_and_runs_on() {
        let start = timeline().last().unwrap().0;
        let before = calls(&at(start - ms(1)));
        assert!(before.iter().all(|(t, _)| t != "bash"));
        let building = calls(&at(start));
        assert_eq!(
            building.last().unwrap(),
            &("bash".into(), CallState::Running)
        );
        assert!(
            building[..building.len() - 1]
                .iter()
                .all(|(_, s)| *s != CallState::Running)
        );
        // Only the build is left, and it waits for the turn to end.
        assert_eq!(next(start), None);
        assert_eq!(at(start + Duration::from_secs(60)), at(start));
    }

    #[test]
    fn next_names_the_time_of_the_next_change() {
        assert_eq!(next(ms(0)), Some(GLOB));
        let mut t = ms(0);
        while let Some(step) = next(t) {
            assert!(at(t) != at(t + step), "nothing changed at {:?}", t + step);
            t += step;
        }
    }

    #[test]
    fn a_completed_script_settles_every_call_and_returns() {
        let Row::Script {
            state,
            calls,
            logs,
            result,
            summary,
            ..
        } = settle(ms(500), Settle::Completed)
        else {
            unreachable!()
        };
        assert_eq!(state, CallState::Done);
        assert_eq!(calls.len(), MANIFESTS + 2);
        assert_eq!(calls.last().unwrap().note.as_deref(), Some("exit 0"));
        assert_eq!(logs, ["read 11 of 12"]);
        assert!(result.is_some() && summary.is_none());
    }

    #[test]
    fn an_interrupted_script_fails_what_it_had_started_and_keeps_its_logs() {
        let Row::Script {
            state,
            calls,
            logs,
            result,
            summary,
            ..
        } = settle(GLOB, Settle::Interrupted)
        else {
            unreachable!()
        };
        assert_eq!(
            (state, summary.as_deref(), result),
            (CallState::Failed, Some("interrupted"), None)
        );
        assert_eq!(calls.len(), 1 + POOL);
        assert!(
            calls[1..]
                .iter()
                .all(|c| c.note.as_deref() == Some("interrupted"))
        );
        assert!(logs.is_empty());
        let late = timeline().last().unwrap().0 + ms(10);
        let Row::Script { logs, .. } = settle(late, Settle::Interrupted) else {
            unreachable!()
        };
        assert_eq!(logs, ["read 11 of 12"]);
    }

    fn drawn(row: Row) -> Vec<String> {
        let look = Look {
            tones: Tones::None,
            classic: false,
            lit: true,
        };
        present(&[row], 0, 84, look)
            .iter()
            .map(|line| line.spans.iter().map(|s| s.content.as_ref()).collect())
            .filter(|line: &String| !line.starts_with("  │ ") || line == "  │")
            .collect()
    }

    #[test]
    fn a_running_script_shows_its_newest_calls_and_counts_those_in_flight() {
        assert_eq!(
            drawn(at(ms(900))),
            [
                "  ● Script: Read every manifest, then build  13 calls · 9 running",
                "  │",
                "  ├ ⋯ 8 more calls",
                "  ├ ✗ tools.read  packages/goal/package.json  Permission denied",
                "  ├ ● tools.read  packages/subagent/package.json",
                "  ├ ● tools.read  packages/skill/package.json",
                "  ├ ● tools.read  packages/web/package.json",
                "  └ ● tools.read  packages/mcp/package.json",
            ]
        );
    }

    #[test]
    fn an_interruption_is_not_counted_as_a_failure() {
        let lines = drawn(settle(ms(900), Settle::Interrupted));
        assert_eq!(
            lines[0],
            "  ✗ Script: Read every manifest, then build  13 calls · 1 failed   interrupted "
        );
        // The failure that was news stays; the stopped calls fold like any.
        assert!(
            lines.contains(&"  ├ ⋯ 6 more calls".to_owned()),
            "{lines:#?}"
        );
        assert!(lines.iter().any(|l| l.ends_with("Permission denied")));
        assert!(
            lines
                .last()
                .unwrap()
                .ends_with("mcp/package.json  interrupted")
        );
    }
}
