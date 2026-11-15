//! A run: every sample of a paired evaluation, its records, and its report.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use bake_process::ProcessSpec;
use tokio::sync::Semaphore;

use crate::process;
use crate::report::{self, Executable, RunRecord};
use crate::sample::{self, Arm, Context, ModelSpec, Sample, SampleDirs, SampleSpec};
use crate::task::{self, Task};

/// The sample records of a run, one JSON object a line.
pub const SAMPLES_FILE: &str = "samples.jsonl";
/// The run's parameters.
pub const RUN_FILE: &str = "run.json";
/// The report.
pub const REPORT_FILE: &str = "README.md";

/// What `bake-eval run` was asked for.
#[derive(Debug, Clone)]
pub struct RunOptions {
    /// The record's name.
    pub name: String,
    /// The base executable.
    pub base: PathBuf,
    /// The candidate executable.
    pub candidate: PathBuf,
    /// The models.
    pub models: Vec<ModelSpec>,
    /// The task directory.
    pub tasks: PathBuf,
    /// Task names to run; empty for all.
    pub only: Vec<String>,
    /// Trials of each task and model by each arm.
    pub trials: u32,
    /// Samples to run at once.
    pub jobs: usize,
    /// The provider whose account the samples use.
    pub provider: String,
    /// The Bake home that account is copied from.
    pub source_home: PathBuf,
    /// `--max-model-calls` for each run.
    pub max_model_calls: u64,
    /// `--run-timeout-ms` for each run.
    pub run_timeout_ms: u64,
    /// Where the records go; it must not already hold a run.
    pub out: PathBuf,
    /// Where sample directories are made; a temporary directory when `None`.
    pub work: Option<PathBuf>,
    /// Whether sample directories are kept, without their accounts.
    pub keep_work: bool,
    /// A JSON file of `settings.json` members for the base arm's samples.
    pub base_settings: Option<PathBuf>,
    /// The same for the candidate arm's samples.
    pub candidate_settings: Option<PathBuf>,
}

/// A directory under the system's temporary directory that no earlier run
/// of this process or another made.
fn scratch_dir(label: &str) -> Result<PathBuf, String> {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos());
    let dir =
        std::env::temp_dir().join(format!("bake-eval-{label}-{}-{nanos}", std::process::id()));
    fs::create_dir_all(&dir)
        .map_err(|error| format!("cannot create {}: {error}", dir.display()))?;
    Ok(dir)
}

async fn executable(path: &Path) -> Result<Executable, String> {
    let metadata = fs::metadata(path)
        .map_err(|error| format!("cannot read the executable {}: {error}", path.display()))?;
    if !metadata.is_file() {
        return Err(format!("{} is not a file", path.display()));
    }
    let spec = ProcessSpec {
        program: path.into(),
        args: vec!["--version".into()],
        cwd: None,
        env: None,
        stdin: false,
    };
    let finished = process::run(&spec, Duration::from_secs(30), 4096)
        .await
        .map_err(|error| format!("cannot run {}: {error}", path.display()))?;
    let version = String::from_utf8_lossy(&finished.stdout).trim().to_owned();
    if finished.code != Some(0) || version.is_empty() {
        return Err(format!(
            "{} --version did not print a version",
            path.display()
        ));
    }
    Ok(Executable {
        path: path.display().to_string(),
        version: version.lines().next().unwrap_or_default().to_owned(),
        bytes: metadata.len(),
    })
}

/// The samples in the order they start: by trial, task, and model, each
/// pair's two arms next to each other, the first arm alternating so that
/// neither arm always runs first.
fn schedule(options: &RunOptions, tasks: &[Arc<Task>]) -> Vec<SampleSpec> {
    let mut specs = Vec::new();
    let mut pair = 0usize;
    for trial in 1..=options.trials {
        for task in tasks {
            for model in &options.models {
                let arms = if pair.is_multiple_of(2) {
                    [Arm::Base, Arm::Candidate]
                } else {
                    [Arm::Candidate, Arm::Base]
                };
                pair += 1;
                for arm in arms {
                    specs.push(SampleSpec {
                        task: Arc::clone(task),
                        model: model.clone(),
                        arm,
                        trial,
                        exe: match arm {
                            Arm::Base => options.base.clone(),
                            Arm::Candidate => options.candidate.clone(),
                        },
                    });
                }
            }
        }
    }
    specs
}

fn write_file(path: &Path, text: &str) -> Result<(), String> {
    fs::write(path, text).map_err(|error| format!("cannot write {}: {error}", path.display()))
}

/// Reads the samples of the run in `out`.
pub fn read_samples(out: &Path) -> Result<Vec<Sample>, String> {
    let path = out.join(SAMPLES_FILE);
    let text = fs::read_to_string(&path)
        .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .enumerate()
        .map(|(index, line)| {
            serde_json::from_str(line)
                .map_err(|error| format!("{} line {}: {error}", path.display(), index + 1))
        })
        .collect()
}

/// Writes the report of the run in `out` from its records.
pub fn write_report(out: &Path) -> Result<PathBuf, String> {
    let path = out.join(RUN_FILE);
    let text = fs::read_to_string(&path)
        .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    let run: RunRecord =
        serde_json::from_str(&text).map_err(|error| format!("{}: {error}", path.display()))?;
    let samples = read_samples(out)?;
    let page = out.join(REPORT_FILE);
    write_file(&page, &report::render(&run, &samples))?;
    Ok(page)
}

/// Runs the evaluation and writes its records and report into
/// `options.out`. Sample records are appended as samples finish, so a run
/// that is stopped keeps what it measured.
pub async fn run(mut options: RunOptions) -> Result<PathBuf, String> {
    if options.trials == 0 || options.jobs == 0 || options.models.is_empty() {
        return Err("a run needs at least one model, one trial, and one job".to_owned());
    }
    // A sample runs in its own workspace, where a path relative to this
    // process's directory would not name the executable.
    for path in [&mut options.base, &mut options.candidate] {
        *path = fs::canonicalize(&*path)
            .map_err(|error| format!("cannot read the executable {}: {error}", path.display()))?;
    }
    let tasks: Vec<Arc<Task>> = task::load_tasks(&options.tasks, &options.only)?
        .into_iter()
        .map(Arc::new)
        .collect();
    let base = executable(&options.base).await?;
    let candidate = executable(&options.candidate).await?;
    let same_executable = options.base == options.candidate;
    let arm_settings = |path: &Option<PathBuf>| match path {
        Some(path) => crate::home::read_arm_settings(path),
        None => Ok(serde_json::Map::new()),
    };
    let base_settings = arm_settings(&options.base_settings)?;
    let candidate_settings = arm_settings(&options.candidate_settings)?;
    if options.out.join(SAMPLES_FILE).exists() {
        return Err(format!(
            "{} already holds a run; choose another directory",
            options.out.display()
        ));
    }
    fs::create_dir_all(&options.out)
        .map_err(|error| format!("cannot create {}: {error}", options.out.display()))?;
    let work = match &options.work {
        Some(work) => {
            fs::create_dir_all(work)
                .map_err(|error| format!("cannot create {}: {error}", work.display()))?;
            work.clone()
        }
        None => scratch_dir("run")?,
    };
    let record = RunRecord {
        name: options.name.clone(),
        date: report::utc_date(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |elapsed| elapsed.as_secs()),
        ),
        base,
        candidate,
        same_executable,
        base_settings: base_settings.clone(),
        candidate_settings: candidate_settings.clone(),
        models: options.models.iter().map(ModelSpec::label).collect(),
        tasks: tasks
            .iter()
            .map(|task| (task.name.clone(), task.prompt.clone()))
            .collect(),
        trials: options.trials,
        jobs: options.jobs,
        max_model_calls: options.max_model_calls,
        run_timeout_ms: options.run_timeout_ms,
        host: format!("{}/{}", std::env::consts::OS, std::env::consts::ARCH),
    };
    let record_text = serde_json::to_string_pretty(&record)
        .map_err(|error| format!("cannot write the run record: {error}"))?;
    write_file(&options.out.join(RUN_FILE), &format!("{record_text}\n"))?;

    let context = Arc::new(Context {
        work: work.clone(),
        source_home: options.source_home.clone(),
        provider: options.provider.clone(),
        max_model_calls: options.max_model_calls,
        run_timeout_ms: options.run_timeout_ms,
        after_run_limit: sample::AFTER_RUN_LIMIT,
        keep_work: options.keep_work,
        base_settings,
        candidate_settings,
    });
    let specs = schedule(&options, &tasks);
    let total = specs.len();
    let samples_path = options.out.join(SAMPLES_FILE);
    let mut samples_file = fs::File::create(&samples_path)
        .map_err(|error| format!("cannot write {}: {error}", samples_path.display()))?;
    // Samples start in schedule order as slots free up, on a task of their
    // own, so that this one records each as soon as it and those before it
    // have finished.
    let slots = Arc::new(Semaphore::new(options.jobs));
    let (started, mut running) = tokio::sync::mpsc::unbounded_channel();
    let starter = tokio::spawn(async move {
        for spec in specs {
            let Ok(slot) = Arc::clone(&slots).acquire_owned().await else {
                return;
            };
            let context = Arc::clone(&context);
            let sample = tokio::spawn(async move {
                let sample = sample::run_sample(&spec, &context).await;
                drop(slot);
                sample
            });
            if started.send(sample).is_err() {
                return;
            }
        }
    });
    let mut index = 0;
    while let Some(sample) = running.recv().await {
        let sample = sample
            .await
            .map_err(|error| format!("a sample's task failed: {error}"))?;
        let line = serde_json::to_string(&sample)
            .map_err(|error| format!("cannot write a sample record: {error}"))?;
        writeln!(samples_file, "{line}")
            .and_then(|()| samples_file.flush())
            .map_err(|error| format!("cannot write {}: {error}", samples_path.display()))?;
        index += 1;
        eprintln!(
            "[{index}/{total}] {} {} {} trial {}: {}{}",
            sample.task,
            sample.model,
            sample.arm.name(),
            sample.trial,
            if sample.passed { "passed" } else { "FAILED" },
            if sample.measured {
                format!(
                    ", {} requests, {} tokens",
                    sample.model_calls, sample.total_tokens
                )
            } else {
                format!(" ({})", sample.outcome)
            }
        );
    }
    starter
        .await
        .map_err(|error| format!("the sample starter failed: {error}"))?;
    if !options.keep_work && options.work.is_none() {
        let _ = fs::remove_dir_all(&work);
    }
    write_report(&options.out)
}

/// Proves each task's check without a model: it must fail on the untouched
/// workspace with an empty answer, and pass once the task's solution is
/// copied over it. Returns one line per problem.
pub async fn check_tasks(tasks: &[Task]) -> Result<Vec<String>, String> {
    let work = scratch_dir("check")?;
    let mut problems = Vec::new();
    for task in tasks {
        let dirs = match SampleDirs::create(work.join(&task.name), task).await {
            Ok(dirs) => dirs,
            Err(error) => {
                problems.push(format!("{}: {error}", task.name));
                continue;
            }
        };
        match dirs.check(task).await {
            Ok(Some(_)) => {}
            Ok(None) => problems.push(format!(
                "{}: check.sh passes on the untouched workspace",
                task.name
            )),
            Err(error) => problems.push(format!("{}: {error}", task.name)),
        }
        let Some(solution) = task.solution() else {
            problems.push(format!("{}: no solution directory", task.name));
            continue;
        };
        let applied = task::copy_tree(&solution, &dirs.workspace).and_then(|()| {
            let answer = dirs.workspace.join(task::SOLUTION_ANSWER);
            if answer.is_file() {
                fs::rename(&answer, &dirs.answer)?;
            }
            Ok(())
        });
        if let Err(error) = applied {
            problems.push(format!("{}: cannot apply the solution: {error}", task.name));
            continue;
        }
        match dirs.check(task).await {
            Ok(None) => {}
            Ok(Some(failure)) => problems.push(format!(
                "{}: check.sh fails on the solution: {failure}",
                task.name
            )),
            Err(error) => problems.push(format!("{}: {error}", task.name)),
        }
    }
    let _ = fs::remove_dir_all(&work);
    Ok(problems)
}
