//! A scripted stand-in for the agent runtime, so the preview's transcript,
//! bar, and composer modes can be driven before a native runtime exists. It
//! answers every prompt with the same short turn, streamed on a fixed
//! timeline: reasoning, one call, then an answer. No model or tool runs, and
//! the answer says so. It also reports sample tasks, subagents and
//! background shells, for the tasks row under the composer; none of them
//! runs either, and each says so. It is development tooling and proves no
//! parity.
//!
//! [`Fixture`] is pure: it is told the time and returns the updates due by
//! then. [`crate::port::FixturePort`] drives it from a thread.

use std::collections::VecDeque;
use std::time::Duration;

use bake_tui_view::agents::{Agent, AgentState, Ended};
use bake_tui_view::jobs::{BackgroundJob, JobState};
use bake_tui_view::runtime::{Pending, RuntimeUpdate, Submission, Target};
use bake_tui_view::state::Outcome;
use bake_tui_view::transcript::{CallState, Row};

/// Time between streamed words.
pub const WORD: Duration = Duration::from_millis(40);
/// How long the turn's one call runs.
pub const CALL: Duration = Duration::from_millis(800);

const ANSWER: &str = "This reply comes from the preview's fixture runtime. It streams in \
pieces and is committed once; no model or tool ran.";

/// What every sample shell's output starts with.
const SAMPLE_OUTPUT: &str = "This is sample output; no command ran.";

/// The scripted runtime: the step running, if any, the steers waiting for
/// the next step boundary, and the sample tasks.
#[derive(Debug, Default)]
pub struct Fixture {
    step: Option<Step>,
    steers: VecDeque<String>,
    turns: u32,
    tasks: Samples,
}

/// The sample tasks as last reported, and when: a "running" shell's time
/// goes on from then, as the view's clock for it does.
#[derive(Debug, Default)]
struct Samples {
    agents: Vec<Agent>,
    jobs: Vec<BackgroundJob>,
    at: Duration,
}

impl Samples {
    fn new(at: Duration) -> Self {
        let agent = |id: &str, name: &str, state, detail: Option<&str>| Agent {
            id: id.into(),
            name: name.into(),
            state,
            route: None,
            detail: detail.map(str::to_owned),
            file: None,
        };
        let shell = |id: &str, command: &str, state, seconds, more: &str| BackgroundJob {
            id: id.into(),
            command: command.into(),
            state,
            elapsed: Duration::from_secs(seconds),
            output: format!("{SAMPLE_OUTPUT}\n{more}"),
            omitted_bytes: 0,
        };
        Self {
            // In start order, as a runtime reports them.
            agents: vec![
                agent(
                    "sample-reviewer",
                    "Sample reviewer",
                    AgentState::Idle(Some(Ended::Completed)),
                    Some("A sample that shows a finished agent; no model ran."),
                ),
                agent(
                    "sample-tester",
                    "Sample tester",
                    AgentState::Saved(Some(Ended::Failed)),
                    Some("A sample that shows a failed agent; no model ran."),
                ),
                agent(
                    "sample-explorer",
                    "Sample explorer",
                    AgentState::Working,
                    Some("A sample that shows an agent at work; no model runs."),
                ),
            ],
            jobs: vec![
                shell(
                    "bash-1",
                    "cargo build --release",
                    JobState::Completed(Some("exit code: 0".into())),
                    41,
                    "",
                ),
                shell(
                    "bash-2",
                    "bun run dev",
                    JobState::Running,
                    72,
                    "It shows as running until x stops it.\n",
                ),
            ],
            at,
        }
    }

    fn report(&self) -> Vec<RuntimeUpdate> {
        vec![
            RuntimeUpdate::Agents(self.agents.clone()),
            RuntimeUpdate::Jobs(self.jobs.clone()),
        ]
    }
}

/// One answered prompt: its updates on a timeline from `start`, how far it
/// has got, and the rows it has shown live, to settle if it is stopped.
#[derive(Debug)]
struct Step {
    start: Duration,
    updates: Vec<(Duration, RuntimeUpdate)>,
    next: usize,
    live: Vec<Row>,
}

impl Fixture {
    /// Reports the sample tasks, as a runtime reports a session's tasks
    /// when it opens.
    pub fn open(&mut self, now: Duration) -> Vec<RuntimeUpdate> {
        self.tasks = Samples::new(now);
        self.tasks.report()
    }

    /// Stops the sample shell `id` while it shows as running: its time
    /// stops where the view's clock had it.
    pub fn stop_job(&mut self, id: &str, now: Duration) -> Vec<RuntimeUpdate> {
        let tasks = &mut self.tasks;
        if !tasks
            .jobs
            .iter()
            .any(|job| job.id == id && job.state == JobState::Running)
        {
            return Vec::new();
        }
        // Each report restarts the view's clocks from the times it holds.
        let ran = now.saturating_sub(tasks.at);
        for job in &mut tasks.jobs {
            if job.state == JobState::Running {
                job.elapsed += ran;
            }
            if job.id == id {
                job.state = JobState::Killed(None);
            }
        }
        tasks.at = now;
        vec![RuntimeUpdate::Jobs(tasks.jobs.clone())]
    }

    /// Stops the sample agent `id` while it shows as working.
    pub fn stop_agent(&mut self, id: &str) -> Vec<RuntimeUpdate> {
        let Some(agent) = self
            .tasks
            .agents
            .iter_mut()
            .find(|agent| agent.id == id && agent.state == AgentState::Working)
        else {
            return Vec::new();
        };
        agent.state = AgentState::Idle(Some(Ended::Stopped));
        agent.detail = Some("A sample stopped before it ended; no model ran.".into());
        vec![RuntimeUpdate::Agents(self.tasks.agents.clone())]
    }

    /// Takes a prompt. A follow-up starts a turn; a steer waits for the
    /// running turn's next step. Either arriving on the other side of a
    /// turn's end is taken the other way, as the turn now stands.
    pub fn submit(&mut self, submission: Submission, now: Duration) -> Vec<RuntimeUpdate> {
        let (Submission::FollowUp(text) | Submission::Steer(text)) = submission;
        if self.step.is_some() {
            self.steers.push_back(text);
            return vec![self.pending()];
        }
        self.start_turn(vec![text], now)
    }

    /// Sends every waiting steer now, as Alt+↑ does: the
    /// running turn is interrupted, keeping them, and a new turn starts with
    /// all of them, in the order they were sent.
    pub fn send_pending(&mut self, now: Duration) -> Vec<RuntimeUpdate> {
        if self.steers.is_empty() {
            return Vec::new();
        }
        let batch: Vec<String> = self.steers.drain(..).collect();
        let mut updates = self.stop();
        updates.push(self.pending());
        updates.extend(self.start_turn(batch, now));
        updates
    }

    /// The waiting steers as the runtime reports them.
    fn pending(&self) -> RuntimeUpdate {
        RuntimeUpdate::Pending(
            self.steers
                .iter()
                .map(|text| Pending {
                    text: text.clone(),
                    target: Target::NextStep,
                })
                .collect(),
        )
    }

    /// Stops the running turn where it is: what was live is committed as it
    /// stood, a running call marked interrupted. Waiting steers are kept and
    /// start the next turn.
    pub fn cancel(&mut self, now: Duration) -> Vec<RuntimeUpdate> {
        let mut updates = self.stop();
        if !updates.is_empty()
            && let Some(text) = self.steers.pop_front()
        {
            updates.push(self.pending());
            updates.extend(self.start_turn(vec![text], now));
        }
        updates
    }

    /// Ends the running turn where it is, if one runs.
    fn stop(&mut self) -> Vec<RuntimeUpdate> {
        let Some(step) = self.step.take() else {
            return Vec::new();
        };
        let mut updates = Vec::new();
        if !step.live.is_empty() {
            updates.push(RuntimeUpdate::Commit(
                step.live.into_iter().map(interrupted).collect(),
            ));
        }
        updates.push(RuntimeUpdate::TurnEnded(Outcome::Interrupted));
        updates
    }

    /// The updates due by `now`, in order. A step that has run out takes
    /// the next waiting steer, or else ends the turn.
    pub fn advance(&mut self, now: Duration) -> Vec<RuntimeUpdate> {
        let mut out = Vec::new();
        while let Some(step) = &mut self.step {
            while let Some((at, update)) = step.updates.get(step.next) {
                if step.start + *at > now {
                    return out;
                }
                match update {
                    RuntimeUpdate::Live(rows) => step.live.clone_from(rows),
                    RuntimeUpdate::Commit(_) => step.live.clear(),
                    _ => {}
                }
                out.push(update.clone());
                step.next += 1;
            }
            // The step has ended at its last update's time.
            let end = step.start + step.updates.last().map_or(Duration::ZERO, |(at, _)| *at);
            self.step = None;
            match self.steers.pop_front() {
                Some(text) => {
                    self.step = Some(step_for(vec![text], end));
                    out.push(self.pending());
                }
                None => out.push(RuntimeUpdate::TurnEnded(Outcome::Completed)),
            }
        }
        out
    }

    /// When the next update is due, on the clock `advance` is given.
    pub fn next_due(&self) -> Option<Duration> {
        let step = self.step.as_ref()?;
        step.updates.get(step.next).map(|(at, _)| step.start + *at)
    }

    fn start_turn(&mut self, prompts: Vec<String>, now: Duration) -> Vec<RuntimeUpdate> {
        self.turns += 1;
        // The step's first update commits the prompts; it is sent at once,
        // before the turn starts, rather than on the step's timeline.
        let mut step = step_for(prompts, now);
        let commit = step.updates.remove(0).1;
        self.step = Some(step);
        vec![
            commit,
            RuntimeUpdate::TurnStarted {
                seed: format!("fixture-{}", self.turns),
            },
        ]
    }
}

/// The script for one step, from `start`: its prompts committed in order,
/// then reasoning streamed a word at a time, one call that runs for
/// [`CALL`], and the answer streamed the same way. The reasoning quotes the
/// last prompt.
fn step_for(prompts: Vec<String>, start: Duration) -> Step {
    let prompt = prompts.last().cloned().unwrap_or_default();
    let mut updates = vec![(
        Duration::ZERO,
        RuntimeUpdate::Commit(prompts.into_iter().map(Row::User).collect()),
    )];
    let mut at = Duration::ZERO;
    updates.push((at, RuntimeUpdate::Phase("thinking".into())));
    let reasoning = format!(
        "The fixture answers \u{201c}{}\u{201d} from its script.",
        brief(&prompt)
    );
    stream(&mut updates, &mut at, &reasoning, Row::Reasoning);
    let call = |state, summary: Option<&str>| Row::Call {
        tool: "Bash".into(),
        argument: "true".into(),
        fields: Vec::new(),
        id: None,
        state,
        summary: summary.map(str::to_owned),
        output: Vec::new(),
        full_output_path: None,
    };
    updates.push((at, RuntimeUpdate::Commit(vec![Row::Reasoning(reasoning)])));
    updates.push((at, RuntimeUpdate::Phase("running true".into())));
    updates.push((
        at,
        RuntimeUpdate::Live(vec![call(CallState::Running, None)]),
    ));
    at += CALL;
    updates.push((
        at,
        RuntimeUpdate::Commit(vec![call(CallState::Done, Some("exit 0"))]),
    ));
    updates.push((at, RuntimeUpdate::Phase("writing".into())));
    stream(&mut updates, &mut at, ANSWER, Row::Answer);
    updates.push((at, RuntimeUpdate::Commit(vec![Row::Answer(ANSWER.into())])));
    Step {
        start,
        updates,
        next: 0,
        live: Vec::new(),
    }
}

/// Adds `text` as live rows growing a word at a time, [`WORD`] apart.
fn stream(
    updates: &mut Vec<(Duration, RuntimeUpdate)>,
    at: &mut Duration,
    text: &str,
    row: fn(String) -> Row,
) {
    let mut end = 0;
    for word in text.split_inclusive(' ') {
        end += word.len();
        updates.push((
            *at,
            RuntimeUpdate::Live(vec![row(text[..end].trim_end().to_owned())]),
        ));
        *at += WORD;
    }
}

/// The prompt's first line, cut to 40 characters, for the reasoning to quote.
fn brief(prompt: &str) -> String {
    let line = prompt.lines().next().unwrap_or_default();
    let mut chars = line.chars();
    let cut: String = chars.by_ref().take(40).collect();
    if chars.next().is_some() {
        format!("{cut}\u{2026}")
    } else {
        cut
    }
}

/// A live row as an interruption leaves it: a running call stopped and
/// tagged, anything else as far as it got.
fn interrupted(row: Row) -> Row {
    match row {
        Row::Call {
            tool,
            argument,
            fields,
            id,
            state: CallState::Running,
            output,
            full_output_path,
            ..
        } => Row::Call {
            tool,
            argument,
            fields,
            id,
            state: CallState::Failed,
            summary: Some("interrupted".into()),
            output,
            full_output_path,
        },
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LONG: Duration = Duration::from_secs(60);

    fn rows(updates: &[RuntimeUpdate]) -> Vec<Row> {
        // Applies updates to a transcript the way the view does.
        let mut t = bake_tui_view::transcript::Transcript::new(Vec::new());
        for update in updates {
            match update {
                RuntimeUpdate::Live(rows) => t.set_live(rows.clone()),
                RuntimeUpdate::Commit(rows) => t.commit(rows.clone()),
                _ => {}
            }
        }
        t.rows
    }

    fn follow_up(text: &str) -> Submission {
        Submission::FollowUp(text.into())
    }

    #[test]
    fn a_prompt_streams_a_turn_that_commits_each_row_once() {
        let mut f = Fixture::default();
        let first = f.submit(follow_up("hi"), Duration::ZERO);
        assert_eq!(
            first,
            [
                RuntimeUpdate::Commit(vec![Row::User("hi".into())]),
                RuntimeUpdate::TurnStarted {
                    seed: "fixture-1".into()
                },
            ]
        );
        // The first word is due at once; nothing else is.
        let now = f.advance(Duration::ZERO);
        assert_eq!(now[0], RuntimeUpdate::Phase("thinking".into()));
        assert!(matches!(&now[1], RuntimeUpdate::Live(r) if r == &[Row::Reasoning("The".into())]));
        assert_eq!(now.len(), 2);
        assert_eq!(f.next_due(), Some(WORD));

        let rest = f.advance(LONG);
        assert_eq!(
            rest.last(),
            Some(&RuntimeUpdate::TurnEnded(Outcome::Completed))
        );
        assert_eq!(f.next_due(), None);
        let all: Vec<_> = first.into_iter().chain(now).chain(rest).collect();
        let shown = rows(&all);
        assert_eq!(shown.len(), 4, "{shown:#?}");
        assert_eq!(shown[0], Row::User("hi".into()));
        assert_eq!(
            shown[1],
            Row::Reasoning("The fixture answers \u{201c}hi\u{201d} from its script.".into())
        );
        assert!(
            matches!(&shown[2], Row::Call { state: CallState::Done, summary, .. } if summary.as_deref() == Some("exit 0"))
        );
        assert_eq!(shown[3], Row::Answer(ANSWER.into()));
        // Phases follow the work.
        let phases: Vec<_> = all
            .iter()
            .filter_map(|u| match u {
                RuntimeUpdate::Phase(p) => Some(p.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(phases, ["thinking", "running true", "writing"]);
    }

    #[test]
    fn a_steer_joins_the_turn_after_its_current_step() {
        let mut f = Fixture::default();
        f.submit(follow_up("one"), Duration::ZERO);
        assert_eq!(
            f.submit(Submission::Steer("two".into()), WORD),
            [RuntimeUpdate::Pending(vec![Pending {
                text: "two".into(),
                target: Target::NextStep,
            }])]
        );
        let updates = f.advance(LONG);
        let ended = updates
            .iter()
            .filter(|u| matches!(u, RuntimeUpdate::TurnEnded(_)))
            .count();
        assert_eq!(ended, 1, "one turn holds both steps");
        // The steer leaves the pending list when its step starts.
        assert!(updates.contains(&RuntimeUpdate::Pending(Vec::new())));
        let users: Vec<_> = rows(&updates)
            .into_iter()
            .filter(|r| matches!(r, Row::User(_)))
            .collect();
        assert_eq!(users, [Row::User("two".into())]);
    }

    #[test]
    fn a_late_steer_starts_a_turn_and_an_early_follow_up_waits() {
        let mut f = Fixture::default();
        let updates = f.submit(Submission::Steer("late".into()), Duration::ZERO);
        assert!(matches!(updates[1], RuntimeUpdate::TurnStarted { .. }));
        assert!(
            matches!(&f.submit(follow_up("early"), WORD)[..], [RuntimeUpdate::Pending(p)] if p.len() == 1)
        );
    }

    #[test]
    fn cancel_commits_what_was_live_and_keeps_waiting_steers() {
        let mut f = Fixture::default();
        let mut all = f.submit(follow_up("hi"), Duration::ZERO);
        // Into the call: the reasoning has committed and the call is live.
        let words = "The fixture answers \u{201c}hi\u{201d} from its script."
            .split(' ')
            .count();
        let in_call = WORD * u32::try_from(words).unwrap() + CALL / 2;
        all.extend(f.advance(in_call));
        f.submit(Submission::Steer("next".into()), in_call);
        let stopped = f.cancel(in_call);
        assert!(matches!(
            &stopped[0],
            RuntimeUpdate::Commit(r) if matches!(&r[0], Row::Call { state: CallState::Failed, summary, .. } if summary.as_deref() == Some("interrupted"))
        ));
        assert_eq!(stopped[1], RuntimeUpdate::TurnEnded(Outcome::Interrupted));
        assert_eq!(stopped[2], RuntimeUpdate::Pending(Vec::new()));
        // The kept steer starts the next turn.
        assert_eq!(
            stopped[3],
            RuntimeUpdate::Commit(vec![Row::User("next".into())])
        );
        assert!(matches!(stopped[4], RuntimeUpdate::TurnStarted { .. }));
        all.extend(stopped);
        let shown = rows(&all);
        assert_eq!(shown.len(), 4, "the interrupted call is drawn once");
        // A cancel with nothing running does nothing.
        f.cancel(in_call);
        assert_eq!(f.cancel(in_call), []);
    }

    #[test]
    fn sending_pending_input_interrupts_and_starts_a_turn_with_all_of_it() {
        let mut f = Fixture::default();
        assert_eq!(f.send_pending(Duration::ZERO), [], "nothing waits");
        let mut all = f.submit(follow_up("one"), Duration::ZERO);
        all.extend(f.advance(WORD));
        f.submit(Submission::Steer("two".into()), WORD);
        f.submit(Submission::Steer("three".into()), WORD);
        let sent = f.send_pending(WORD);
        assert!(matches!(&sent[0], RuntimeUpdate::Commit(_)), "{sent:#?}");
        assert_eq!(sent[1], RuntimeUpdate::TurnEnded(Outcome::Interrupted));
        assert_eq!(sent[2], RuntimeUpdate::Pending(Vec::new()));
        assert_eq!(
            sent[3],
            RuntimeUpdate::Commit(vec![Row::User("two".into()), Row::User("three".into())])
        );
        assert!(matches!(sent[4], RuntimeUpdate::TurnStarted { .. }));
        all.extend(sent);
        all.extend(f.advance(LONG));
        let users = rows(&all)
            .into_iter()
            .filter(|r| matches!(r, Row::User(_)))
            .count();
        assert_eq!(users, 3);
        assert_eq!(f.next_due(), None);
    }

    // The preview's sample tasks say they are samples, stop as a runtime's
    // would, and keep a stopped shell's time where the view's clock had it.
    #[test]
    fn the_sample_tasks_say_so_and_stop_like_real_ones() {
        let mut f = Fixture::default();
        let opened = f.open(Duration::from_secs(1));
        let [RuntimeUpdate::Agents(agents), RuntimeUpdate::Jobs(jobs)] = &opened[..] else {
            panic!("{opened:#?}");
        };
        assert_eq!(agents.len(), 3);
        assert!(agents.iter().all(|agent| {
            agent.route.is_none()
                && agent
                    .detail
                    .as_deref()
                    .is_some_and(|d| d.contains("no model"))
        }));
        assert!(jobs.iter().all(|job| job.output.starts_with(SAMPLE_OUTPUT)));
        // Only what shows as running stops.
        assert_eq!(f.stop_job("bash-1", LONG), []);
        assert_eq!(f.stop_agent("sample-reviewer"), []);
        let [RuntimeUpdate::Jobs(jobs)] = &f.stop_job("bash-2", Duration::from_secs(4))[..] else {
            panic!("no report");
        };
        let dev = jobs.iter().find(|job| job.id == "bash-2").unwrap();
        assert_eq!(dev.state, JobState::Killed(None));
        assert_eq!(dev.elapsed, Duration::from_secs(75));
        assert_eq!(f.stop_job("bash-2", LONG), [], "already stopped");
        let [RuntimeUpdate::Agents(agents)] = &f.stop_agent("sample-explorer")[..] else {
            panic!("no report");
        };
        assert!(agents.iter().any(|agent| agent.id == "sample-explorer"
            && agent.state == AgentState::Idle(Some(Ended::Stopped))));
    }

    #[test]
    fn a_long_prompt_is_quoted_by_its_first_line() {
        assert_eq!(brief("short\nmore"), "short");
        assert_eq!(
            brief(&"a".repeat(41)),
            format!("{}\u{2026}", "a".repeat(40))
        );
    }
}
