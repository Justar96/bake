//! The `codemode` call a sample turn runs, played on the preview's clock.
//!
//! Nothing runs: the script's calls follow a fixed timeline, so the same
//! elapsed time always draws the same row. It shows how a script's calls
//! reach the screen as they start and settle: one `find`, then a read of
//! every manifest under `Promise.all`, at most [`POOL`] at a time, then a
//! build that runs until the turn ends. What the script hands back appears
//! only when it settles, because the result carries it.

use std::time::Duration;

use crate::codemode::{Codemode, NestedCall, NestedStatus};
use crate::copy;
use crate::transcript::{CallState, Row};

/// Calls a script has running at once; Bake's codemode limit.
pub const POOL: usize = 16;
/// Manifests the sample's `find` finds.
const MANIFESTS: usize = 20;
/// The manifest the script may not read; it catches the error and goes on.
const DENIED: usize = 7;
const FIND: Duration = Duration::from_millis(400);

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
    "session",
    "sandbox",
    "hooks",
    "plugins",
    "permissions",
    "compaction",
    "prompts",
    "settings",
];

/// The script the sample turn runs.
const CODE: &str = "\
const paths = (await tools.find({ pattern: \"packages/*/package.json\" })).split(\"\\n\");
const texts = await Promise.all(
  paths.map((path) => tools.read({ path }).catch(() => \"\")),
);
console.log(`read ${texts.filter(Boolean).length} of ${paths.length}`);
await tools.bash({ command: \"bun run build\" });
return { manifests: paths.length };";

/// How a turn's script is settled: run to its end, or stopped by Esc.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Settle {
    Completed,
    Interrupted,
}

/// When each call starts and, but for the build, ends: the `find`, the
/// reads in the order they start, then the build. A read waits for the
/// earliest slot of the pool to free, as waiting calls start in the order
/// the script made them.
fn timeline() -> Vec<(Duration, Option<Duration>)> {
    let mut calls = vec![(Duration::ZERO, Some(FIND))];
    let mut free = [FIND; POOL];
    let mut last = FIND;
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

/// A call's name and its arguments as the tool records them.
fn call(index: usize) -> (&'static str, String) {
    match index {
        0 => ("find", r#"{"pattern":"packages/*/package.json"}"#.into()),
        i if i <= MANIFESTS => (
            "read",
            format!(r#"{{"path":"packages/{}/package.json"}}"#, PACKAGES[i - 1]),
        ),
        _ => (
            "bash",
            format!(r#"{{"command":"{}"}}"#, copy::SAMPLE_COMMAND),
        ),
    }
}

/// Call `index` as it stands: running, or settled after `duration`, the
/// denied read with its error.
fn nested(index: usize, settled: Option<Duration>) -> NestedCall {
    let (name, args) = call(index);
    let denied = index == DENIED + 1;
    NestedCall {
        name: name.into(),
        args,
        status: match settled {
            None => NestedStatus::Running,
            Some(_) if denied => NestedStatus::Error,
            Some(_) => NestedStatus::Ok,
        },
        duration: settled,
        error: (settled.is_some() && denied).then(|| "Permission denied".into()),
    }
}

fn script(state: CallState, calls: Vec<NestedCall>) -> Codemode {
    Codemode {
        code: CODE.into(),
        state,
        summary: None,
        calls,
        output: Vec::new(),
        full_output_path: None,
        wall_time: None,
    }
}

/// The script as it stands `elapsed` after the turn started: every call
/// started so far, those still running blinking.
pub fn at(elapsed: Duration) -> Row {
    let calls = timeline()
        .into_iter()
        .enumerate()
        .take_while(|(_, (start, _))| *start <= elapsed)
        .map(|(index, (start, end))| match end {
            Some(end) if end <= elapsed => nested(index, Some(end - start)),
            _ => nested(index, None),
        })
        .collect();
    Row::Codemode(script(CallState::Running, calls))
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
/// runs to its end: every call settles, the build when the turn ends, and
/// what it logged and returned appears with how long it took. Interrupted,
/// the calls it had started are cancelled, the ones it had not never run,
/// and nothing went back to the model.
pub fn settle(elapsed: Duration, how: Settle) -> Row {
    let timeline = timeline();
    match how {
        Settle::Completed => {
            let calls = timeline
                .iter()
                .enumerate()
                .map(|(index, (start, end))| {
                    let end = end.unwrap_or(elapsed.max(*start));
                    nested(index, Some(end - *start))
                })
                .collect();
            let finished = elapsed.max(timeline.last().map_or(Duration::ZERO, |(start, _)| *start));
            Row::Codemode(Codemode {
                output: vec![
                    format!(r#"{{"manifests":{MANIFESTS}}}"#),
                    "<console_output>".into(),
                    format!("read {} of {MANIFESTS}", MANIFESTS - 1),
                    "</console_output>".into(),
                ],
                wall_time: Some(finished),
                ..script(CallState::Done, calls)
            })
        }
        Settle::Interrupted => {
            let Row::Codemode(stopped) = at(elapsed) else {
                unreachable!("at() draws a codemode call")
            };
            let calls = stopped
                .calls
                .into_iter()
                .map(|call| match call.status {
                    NestedStatus::Running => NestedCall {
                        status: NestedStatus::Cancelled,
                        ..call
                    },
                    _ => call,
                })
                .collect();
            Row::Codemode(Codemode {
                summary: Some(copy::INTERRUPTED_NOTE.into()),
                ..script(CallState::Failed, calls)
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::activity::Tones;
    use crate::codemode::CALL_PREVIEW_COUNT;
    use crate::transcript::{Look, present};

    fn script(row: Row) -> Codemode {
        match row {
            Row::Codemode(script) => script,
            other => panic!("{other:?}"),
        }
    }

    fn calls(row: Row) -> Vec<(String, NestedStatus)> {
        script(row)
            .calls
            .into_iter()
            .map(|call| (call.name, call.status))
            .collect()
    }

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn the_find_runs_first_then_reads_fill_the_pool() {
        assert_eq!(calls(at(ms(0))), [("find".into(), NestedStatus::Running)]);
        let reading = calls(at(FIND));
        assert_eq!(reading.len(), 1 + POOL);
        assert_eq!(reading[0].1, NestedStatus::Ok);
        assert!(
            reading[1..]
                .iter()
                .all(|(name, status)| name == "read" && *status == NestedStatus::Running)
        );
    }

    #[test]
    fn the_build_starts_once_every_read_has_settled_and_runs_on() {
        let start = timeline().last().unwrap().0;
        let before = calls(at(start - ms(1)));
        assert!(before.iter().all(|(name, _)| name != "bash"));
        let building = calls(at(start));
        assert_eq!(
            building.last().unwrap(),
            &("bash".into(), NestedStatus::Running)
        );
        assert!(
            building[..building.len() - 1]
                .iter()
                .all(|(_, status)| *status != NestedStatus::Running)
        );
        // Only the build is left, and it waits for the turn to end.
        assert_eq!(next(start), None);
        assert_eq!(at(start + Duration::from_secs(60)), at(start));
    }

    #[test]
    fn next_names_the_time_of_the_next_change() {
        assert_eq!(next(ms(0)), Some(FIND));
        let mut t = ms(0);
        while let Some(step) = next(t) {
            assert!(at(t) != at(t + step), "nothing changed at {:?}", t + step);
            t += step;
        }
    }

    #[test]
    fn a_completed_script_settles_every_call_and_returns() {
        let end = timeline().last().unwrap().0 + ms(900);
        let done = script(settle(end, Settle::Completed));
        assert_eq!(done.state, CallState::Done);
        assert_eq!(done.calls.len(), MANIFESTS + 2);
        assert_eq!(done.calls.last().unwrap().duration, Some(ms(900)));
        assert_eq!(done.output[2], "read 19 of 20");
        assert_eq!(done.wall_time, Some(end));
        assert_eq!(done.summary, None);
    }

    #[test]
    fn an_interrupted_script_cancels_what_it_had_started() {
        let stopped = script(settle(FIND, Settle::Interrupted));
        assert_eq!(
            (stopped.state, stopped.summary.as_deref()),
            (CallState::Failed, Some("interrupted"))
        );
        assert_eq!(stopped.calls.len(), 1 + POOL);
        assert!(
            stopped.calls[1..]
                .iter()
                .all(|call| call.status == NestedStatus::Cancelled)
        );
        assert!(stopped.output.is_empty() && stopped.wall_time.is_none());
    }

    fn drawn(row: Row) -> Vec<String> {
        let look = Look {
            tones: Tones::None,
            classic: false,
            lit: true,
        };
        present(&[row], 0, 84, look)
            .iter()
            .map(|line| line.to_string().trim_end().to_owned())
            .collect()
    }

    #[test]
    fn a_running_script_lists_its_latest_calls() {
        let lines = drawn(at(ms(900)));
        assert_eq!(lines[0], "  ● Codemode  21 calls · 15 running");
        // Seven script lines on eight rows, then the latest calls, the
        // rest counted.
        assert_eq!(lines[9..11], ["  │", "  │ ⋯ 13 earlier calls"]);
        assert_eq!(lines.len(), 11 + CALL_PREVIEW_COUNT);
        assert!(
            lines.contains(
                &r#"  │ ✓ read  {"path":"packages/hooks/package.json"}  300ms"#.to_owned()
            )
        );
    }
}
