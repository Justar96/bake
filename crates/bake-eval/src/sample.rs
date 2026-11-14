//! One sample: one executable on one task and model, then the task's check.

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use bake_process::ProcessSpec;
use serde::{Deserialize, Serialize};

use crate::process::{self, Finished};
use crate::stream;
use crate::task::{self, Task};

/// How long a task's `setup.sh` or `check.sh` may run.
const SCRIPT_LIMIT: Duration = Duration::from_secs(120);

/// Print mode waits up to ten minutes for background jobs after its last
/// prompt; the runner's own limit allows for that, and a minute more.
pub const AFTER_RUN_LIMIT: Duration = Duration::from_secs(11 * 60);

/// The most a run's event stream may hold.
const MAX_STREAM_BYTES: usize = 256 * 1024 * 1024;

/// The most a script's output may hold.
const MAX_SCRIPT_BYTES: usize = 64 * 1024;

/// Which executable a sample ran.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Arm {
    /// The build the change is compared against.
    Base,
    /// The build with the change.
    Candidate,
}

impl Arm {
    /// The arm's name in records and directory names.
    pub fn name(self) -> &'static str {
        match self {
            Self::Base => "base",
            Self::Candidate => "candidate",
        }
    }
}

/// A model and the thinking level to run it at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelSpec {
    /// The model as `provider/id`, which `--model` accepts.
    pub model: String,
    /// The `--thinking` level; `None` leaves Bake's default.
    pub thinking: Option<String>,
}

impl ModelSpec {
    /// Parses `id`, `id:level`, `provider/id`, or `provider/id:level`; an id
    /// without a provider belongs to `provider`.
    pub fn parse(text: &str, provider: &str) -> Result<Self, String> {
        let (id, thinking) = match text.rsplit_once(':') {
            Some((id, level)) => (id, Some(level)),
            None => (text, None),
        };
        if id.is_empty() || thinking.is_some_and(str::is_empty) {
            return Err(format!("invalid model {text:?}: give id or id:level"));
        }
        Ok(Self {
            model: if id.contains('/') {
                id.to_owned()
            } else {
                format!("{provider}/{id}")
            },
            thinking: thinking.map(str::to_owned),
        })
    }

    /// The model as a report names it, such as `cliproxyapi/gpt-5.5:high`.
    pub fn label(&self) -> String {
        match &self.thinking {
            Some(level) => format!("{}:{level}", self.model),
            None => self.model.clone(),
        }
    }
}

/// What to run.
#[derive(Debug, Clone)]
pub struct SampleSpec {
    /// The task.
    pub task: Arc<Task>,
    /// The model.
    pub model: ModelSpec,
    /// The arm.
    pub arm: Arm,
    /// The trial number, from 1. A base and a candidate sample with the same
    /// task, model, and trial are a pair.
    pub trial: u32,
    /// The arm's `bake` executable.
    pub exe: PathBuf,
}

/// What every sample of a run shares.
#[derive(Debug, Clone)]
pub struct Context {
    /// The directory the sample directories are made in.
    pub work: PathBuf,
    /// The Bake home the provider account is copied from.
    pub source_home: PathBuf,
    /// The provider whose account the samples use.
    pub provider: String,
    /// `--max-model-calls` for each run.
    pub max_model_calls: u64,
    /// `--run-timeout-ms` for each run.
    pub run_timeout_ms: u64,
    /// How long past `run_timeout_ms` a run may take before the runner
    /// stops it ([`AFTER_RUN_LIMIT`]).
    pub after_run_limit: Duration,
    /// Whether a sample's directory is kept, without its account.
    pub keep_work: bool,
    /// The `settings.json` members of the base arm's samples.
    pub base_settings: serde_json::Map<String, serde_json::Value>,
    /// The `settings.json` members of the candidate arm's samples.
    pub candidate_settings: serde_json::Map<String, serde_json::Value>,
}

/// The record of one sample, one line of `samples.jsonl`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Sample {
    /// The task's name.
    pub task: String,
    /// The model's label ([`ModelSpec::label`]).
    pub model: String,
    /// The arm.
    pub arm: Arm,
    /// The trial number.
    pub trial: u32,
    /// Whether the run ended in `success` and the task's check passed.
    pub passed: bool,
    /// The result object's `subtype`, or why there is none: `no_result`,
    /// `timed_out`, `setup_failed`, or `start_failed`.
    pub outcome: String,
    /// The run's or the runner's error, or the end of a failed check's
    /// output.
    pub error: Option<String>,
    /// Whether the run printed a result object, so its measures count.
    pub measured: bool,
    /// Model requests admitted.
    pub model_calls: u64,
    /// Agent turns.
    pub turns: u64,
    /// The run's own duration.
    pub duration_ms: u64,
    /// Uncached input tokens.
    pub input: u64,
    /// Output tokens.
    pub output: u64,
    /// Input tokens read from the prompt cache.
    pub cache_read: u64,
    /// Input tokens written to the prompt cache.
    pub cache_write: u64,
    /// Every token the run used.
    pub total_tokens: u64,
    /// The cost, 0 when the model declares no prices.
    pub cost_usd: f64,
    /// Tool calls the model issued.
    pub tool_calls: u64,
    /// Tool calls a tool made for the model.
    pub nested_calls: u64,
    /// Calls of either kind that failed.
    pub tool_errors: u64,
    /// Model responses that called a tool.
    pub tool_responses: u64,
    /// Model responses that called more than one tool.
    pub batched_responses: u64,
}

impl Sample {
    fn unmeasured(spec: &SampleSpec, outcome: &str, error: String) -> Self {
        Self {
            task: spec.task.name.clone(),
            model: spec.model.label(),
            arm: spec.arm,
            trial: spec.trial,
            passed: false,
            outcome: outcome.to_owned(),
            error: Some(error),
            measured: false,
            model_calls: 0,
            turns: 0,
            duration_ms: 0,
            input: 0,
            output: 0,
            cache_read: 0,
            cache_write: 0,
            total_tokens: 0,
            cost_usd: 0.0,
            tool_calls: 0,
            nested_calls: 0,
            tool_errors: 0,
            tool_responses: 0,
            batched_responses: 0,
        }
    }
}

/// The environment of a sample's processes: this process's without its
/// `BAKE_*` variables, with the sample's home and `extra`.
fn environment(home: &Path, extra: &[(&str, &Path)]) -> Vec<(OsString, OsString)> {
    let mut env: Vec<(OsString, OsString)> = std::env::vars_os()
        .filter(|(name, _)| !name.to_string_lossy().starts_with("BAKE_"))
        .collect();
    let mut set = |name: &str, value: OsString| env.push((name.into(), value));
    set("HOME", home.into());
    set("USERPROFILE", home.into());
    set("BAKE_HOME", home.join(".bake").into());
    set("NO_COLOR", "1".into());
    set("PYTHONDONTWRITEBYTECODE", "1".into());
    for (name, value) in extra {
        set(name, (*value).into());
    }
    env
}

/// Runs a task's `script` with `sh` in `workspace`.
pub(crate) async fn run_script(
    task: &Task,
    script: &str,
    workspace: &Path,
    home: &Path,
    answer: &Path,
) -> Result<Finished, String> {
    let spec = ProcessSpec {
        program: "sh".into(),
        args: vec![task.dir.join(script).into()],
        cwd: Some(workspace.to_path_buf()),
        env: Some(environment(
            home,
            &[
                ("BAKE_EVAL_TASK_DIR", &task.dir),
                ("BAKE_EVAL_ANSWER", answer),
            ],
        )),
        stdin: false,
    };
    process::run(&spec, SCRIPT_LIMIT, MAX_SCRIPT_BYTES)
        .await
        .map_err(|error| format!("cannot run {script}: {error}"))
}

fn script_failure(script: &str, finished: &Finished) -> String {
    let output = [finished.stdout.as_slice(), finished.stderr.as_slice()].concat();
    let ending = if finished.timed_out {
        "timed out".to_owned()
    } else {
        match finished.code {
            Some(code) => format!("exited with {code}"),
            None => "was stopped by a signal".to_owned(),
        }
    };
    let tail = process::tail(&output, 600);
    if tail.is_empty() {
        format!("{script} {ending}")
    } else {
        format!("{script} {ending}: {tail}")
    }
}

/// The directories of one sample.
pub(crate) struct SampleDirs {
    pub root: PathBuf,
    pub workspace: PathBuf,
    pub home: PathBuf,
    pub answer: PathBuf,
}

impl SampleDirs {
    /// Makes `root` with the task's workspace and home templates and runs
    /// its setup.
    pub(crate) async fn create(root: PathBuf, task: &Task) -> Result<Self, String> {
        let dirs = Self {
            workspace: root.join("workspace"),
            home: root.join("home"),
            answer: root.join("answer.txt"),
            root,
        };
        let io = |error: std::io::Error| format!("cannot prepare {}: {error}", dirs.root.display());
        if dirs.root.exists() {
            fs::remove_dir_all(&dirs.root).map_err(io)?;
        }
        task::copy_tree(&task.workspace(), &dirs.workspace).map_err(io)?;
        fs::create_dir_all(&dirs.home).map_err(io)?;
        if let Some(template) = task.home() {
            task::copy_tree(&template, &dirs.home).map_err(io)?;
        }
        fs::write(&dirs.answer, "").map_err(io)?;
        if task.setup {
            let finished =
                run_script(task, "setup.sh", &dirs.workspace, &dirs.home, &dirs.answer).await?;
            if finished.timed_out || finished.code != Some(0) {
                return Err(script_failure("setup.sh", &finished));
            }
        }
        Ok(dirs)
    }

    /// Runs the task's check: `Ok(None)` for a pass, else why it failed.
    pub(crate) async fn check(&self, task: &Task) -> Result<Option<String>, String> {
        let finished =
            run_script(task, "check.sh", &self.workspace, &self.home, &self.answer).await?;
        Ok((finished.timed_out || finished.code != Some(0))
            .then(|| script_failure("check.sh", &finished)))
    }
}

fn directory_name(spec: &SampleSpec) -> String {
    let model: String = spec
        .model
        .label()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' {
                c
            } else {
                '-'
            }
        })
        .collect();
    format!(
        "{}_{model}_{}_{}",
        spec.task.name,
        spec.arm.name(),
        spec.trial
    )
}

async fn run_in(spec: &SampleSpec, context: &Context, dirs: &SampleDirs) -> Sample {
    let bake_home = dirs.home.join(".bake");
    let arm_settings = match spec.arm {
        Arm::Base => &context.base_settings,
        Arm::Candidate => &context.candidate_settings,
    };
    if let Err(error) = crate::home::prepare(
        &context.source_home,
        &context.provider,
        &bake_home,
        arm_settings,
    ) {
        return Sample::unmeasured(spec, "start_failed", error);
    }
    let mut args: Vec<OsString> = [
        "-p",
        "--output-format",
        "stream-json",
        "--offline",
        "--model",
        &spec.model.model,
    ]
    .map(OsString::from)
    .to_vec();
    if let Some(level) = &spec.model.thinking {
        args.extend(["--thinking".into(), level.into()]);
    }
    args.extend([
        "--max-model-calls".into(),
        context.max_model_calls.to_string().into(),
        "--run-timeout-ms".into(),
        context.run_timeout_ms.to_string().into(),
        spec.task.prompt.clone().into(),
    ]);
    let process = ProcessSpec {
        program: spec.exe.clone().into(),
        args,
        cwd: Some(dirs.workspace.clone()),
        env: Some(environment(&dirs.home, &[])),
        stdin: false,
    };
    let limit = Duration::from_millis(context.run_timeout_ms) + context.after_run_limit;
    let started = Instant::now();
    let finished = process::run(&process, limit, MAX_STREAM_BYTES).await;
    crate::home::remove_account(&bake_home);
    let finished = match finished {
        Ok(finished) => finished,
        Err(error) => {
            return Sample::unmeasured(
                spec,
                "start_failed",
                format!("cannot run {}: {error}", spec.exe.display()),
            );
        }
    };
    if context.keep_work {
        let _ = fs::write(dirs.root.join("stream.jsonl"), &finished.stdout);
        let _ = fs::write(dirs.root.join("stderr.txt"), &finished.stderr);
    }
    let measures = stream::parse(&finished.stdout);
    let Some(result) = measures.result else {
        let (outcome, error) = if finished.timed_out {
            (
                "timed_out",
                format!("no result after {} s", started.elapsed().as_secs()),
            )
        } else {
            ("no_result", process::tail(&finished.stderr, 600))
        };
        return Sample::unmeasured(spec, outcome, error);
    };
    let _ = fs::write(&dirs.answer, &result.answer);
    let (passed, error) = if result.subtype == "success" {
        match dirs.check(&spec.task).await {
            Ok(None) => (true, None),
            Ok(Some(failure)) | Err(failure) => (false, Some(failure)),
        }
    } else {
        (false, result.error.clone())
    };
    Sample {
        passed,
        outcome: result.subtype,
        error,
        measured: true,
        model_calls: result.model_calls,
        turns: result.turns,
        duration_ms: result.duration_ms,
        input: result.input,
        output: result.output,
        cache_read: result.cache_read,
        cache_write: result.cache_write,
        total_tokens: result.total_tokens,
        cost_usd: result.cost_usd,
        tool_calls: measures.tool_calls,
        nested_calls: measures.nested_calls,
        tool_errors: measures.tool_errors,
        tool_responses: measures.tool_responses,
        batched_responses: measures.batched_responses,
        ..Sample::unmeasured(spec, "", String::new())
    }
}

/// Runs one sample to its record. Nothing it started is left running, and
/// its directory is removed unless the context keeps it.
pub async fn run_sample(spec: &SampleSpec, context: &Context) -> Sample {
    let root = context.work.join(directory_name(spec));
    let sample = match SampleDirs::create(root.clone(), &spec.task).await {
        Ok(dirs) => run_in(spec, context, &dirs).await,
        Err(error) => Sample::unmeasured(spec, "setup_failed", error),
    };
    if !context.keep_work {
        let _ = fs::remove_dir_all(&root);
    }
    sample
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn models_parse_with_and_without_a_provider_or_level() {
        let parse = |text: &str| ModelSpec::parse(text, "route").map(|spec| spec.label());
        assert_eq!(parse("m").as_deref(), Ok("route/m"));
        assert_eq!(parse("m:high").as_deref(), Ok("route/m:high"));
        assert_eq!(parse("other/m:low").as_deref(), Ok("other/m:low"));
        assert_eq!(parse("other/m").as_deref(), Ok("other/m"));
        for bad in ["", ":high", "m:"] {
            assert!(parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_sample_s_environment_has_its_own_home_and_no_inherited_bake_variable() {
        let env = environment(
            Path::new("/sample/home"),
            &[("BAKE_EVAL_TASK_DIR", Path::new("/task"))],
        );
        let last = |name: &str| {
            env.iter()
                .rev()
                .find(|(known, _)| known == name)
                .map(|(_, value)| value.to_string_lossy().into_owned())
        };
        assert_eq!(last("HOME").as_deref(), Some("/sample/home"));
        assert_eq!(last("BAKE_HOME").as_deref(), Some("/sample/home/.bake"));
        assert_eq!(last("BAKE_EVAL_TASK_DIR").as_deref(), Some("/task"));
        let bake: Vec<String> = env
            .iter()
            .map(|(name, _)| name.to_string_lossy().into_owned())
            .filter(|name| name.starts_with("BAKE_"))
            .collect();
        assert_eq!(bake, ["BAKE_HOME", "BAKE_EVAL_TASK_DIR"]);
    }

    #[test]
    fn records_round_trip_as_json_lines() {
        let spec = SampleSpec {
            task: Arc::new(Task {
                name: "t".into(),
                dir: PathBuf::new(),
                prompt: "p".into(),
                setup: false,
            }),
            model: ModelSpec::parse("m:high", "route").expect("model"),
            arm: Arm::Candidate,
            trial: 2,
            exe: PathBuf::new(),
        };
        let sample = Sample {
            passed: true,
            measured: true,
            total_tokens: 12,
            cost_usd: 0.5,
            error: None,
            ..Sample::unmeasured(&spec, "success", String::new())
        };
        let line = serde_json::to_string(&sample).expect("line");
        assert!(line.starts_with(r#"{"task":"t","model":"route/m:high","arm":"candidate","trial":2,"passed":true,"outcome":"success","error":null,"#), "{line}");
        assert_eq!(
            serde_json::from_str::<Sample>(&line).expect("sample"),
            sample
        );
        assert_eq!(directory_name(&spec), "t_route-m-high_candidate_2");
    }
}
