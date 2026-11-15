//! The runner end to end against stand-ins for `bake`: shell scripts the
//! tests write, which print what `bake -p --output-format stream-json`
//! prints. No model is reached, so these check the runner, not a model.
#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use bake_eval::run::{self, RunOptions};
use bake_eval::sample::{self, Arm, Context, ModelSpec, Sample, SampleSpec};
use bake_eval::task;

/// A directory of this test's own, removed when it ends.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("bake-eval-it-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("scratch");
        Self(dir)
    }

    fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }

    fn write(&self, name: &str, text: &str) -> PathBuf {
        let path = self.path(name);
        fs::create_dir_all(path.parent().expect("parent")).expect("dirs");
        fs::write(&path, text).expect("write");
        path
    }

    fn script(&self, name: &str, text: &str) -> PathBuf {
        let path = self.write(name, text);
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).expect("mode");
        path
    }

    /// A Bake home with a signed-in account and settings that must not
    /// reach a sample.
    fn source_home(&self) -> PathBuf {
        self.write(
            "source/auth.json",
            r#"{"cliproxyapi":{"type":"api_key","key":"secret-key","account":"t"}}"#,
        );
        self.write(
            "source/settings.json",
            r#"{"cliproxyapi":{"baseURL":"http://127.0.0.1:9","account":"t"},"telecom":true}"#,
        );
        self.path("source")
    }

    /// One task: the agent must create `done.txt` and answer `hello`.
    fn tasks(&self) -> PathBuf {
        self.write("tasks/touch/prompt.md", "Create done.txt and say hello.\n");
        self.write("tasks/touch/workspace/note.txt", "start\n");
        self.write(
            "tasks/touch/check.sh",
            "set -eu\ntest -f done.txt || { echo 'done.txt is missing'; exit 1; }\ngrep -q hello \"$BAKE_EVAL_ANSWER\"\n",
        );
        self.write("tasks/touch/solution/done.txt", "");
        self.write("tasks/touch/solution/ANSWER.txt", "hello\n");
        self.path("tasks")
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// A stand-in that checks what the runner gave it, does the task, and
/// reports `tokens` total tokens.
fn working_bake(tokens: u64) -> String {
    format!(
        r#"#!/bin/sh
if [ "$1" = "--version" ]; then echo "bake 0.0.0-standin"; exit 0; fi
fail() {{ echo "$1" >&2; exit 1; }}
[ "$BAKE_HOME" = "$HOME/.bake" ] || fail "BAKE_HOME is not in the sample's home"
grep -q secret-key "$BAKE_HOME/auth.json" || fail "the account was not copied"
grep -q telecom "$BAKE_HOME/settings.json" && fail "a user setting was copied"
[ -z "$BAKE_TELECOM" ] || fail "a BAKE_ variable was inherited"
[ -f note.txt ] || fail "not in the workspace"
case "$*" in
  *"--model cliproxyapi/m1 --thinking high --max-model-calls 9 --run-timeout-ms 5000 Create done.txt and say hello."*) ;;
  *) fail "unexpected arguments: $*" ;;
esac
: > done.txt
echo '{{"type":"message_end","message":{{"role":"assistant","content":[{{"type":"toolCall"}},{{"type":"toolCall"}}]}}}}'
echo '{{"type":"tool_execution_end","toolName":"write","isError":false}}'
echo '{{"type":"tool_execution_end","toolName":"bash","isError":true}}'
echo '{{"type":"result","subtype":"success","isError":false,"result":"hello there","error":null,"numTurns":2,"modelCalls":2,"durationMs":1500,"usage":{{"input":10,"output":5,"cacheRead":0,"cacheWrite":0,"totalTokens":{tokens}}},"costUsd":0}}'
"#
    )
}

fn options(scratch: &Scratch, base: PathBuf, candidate: PathBuf) -> RunOptions {
    RunOptions {
        name: "stand-in".into(),
        base,
        candidate,
        models: vec![ModelSpec::parse("m1:high", "cliproxyapi").expect("model")],
        tasks: scratch.tasks(),
        only: Vec::new(),
        trials: 2,
        jobs: 2,
        provider: "cliproxyapi".into(),
        source_home: scratch.source_home(),
        max_model_calls: 9,
        run_timeout_ms: 5000,
        out: scratch.path("out"),
        work: Some(scratch.path("work")),
        keep_work: true,
        base_settings: None,
        candidate_settings: None,
    }
}

fn files_under(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    for entry in fs::read_dir(dir).expect("read_dir") {
        let path = entry.expect("entry").path();
        if path.is_dir() {
            found.extend(files_under(&path));
        } else {
            found.push(path);
        }
    }
    found
}

#[tokio::test(flavor = "multi_thread")]
async fn a_paired_run_writes_its_records_and_report() {
    let scratch = Scratch::new("paired");
    let base = scratch.script("base-bake", &working_bake(1000));
    let candidate = scratch.script("candidate-bake", &working_bake(800));
    let page = run::run(options(&scratch, base, candidate))
        .await
        .expect("run");
    assert_eq!(page, scratch.path("out/README.md"));

    let samples = run::read_samples(&scratch.path("out")).expect("samples");
    assert_eq!(samples.len(), 4);
    for sample in &samples {
        assert!(sample.passed && sample.measured, "{sample:?}");
        assert_eq!(sample.outcome, "success");
        assert_eq!(
            (sample.model_calls, sample.turns, sample.duration_ms),
            (2, 2, 1500)
        );
        assert_eq!((sample.tool_calls, sample.tool_errors), (2, 1));
        assert_eq!((sample.tool_responses, sample.batched_responses), (1, 1));
        let tokens = if sample.arm == Arm::Base { 1000 } else { 800 };
        assert_eq!(sample.total_tokens, tokens);
    }
    // Each pair's arms start next to each other, the first arm alternating.
    let order: Vec<(u32, Arm)> = samples.iter().map(|s| (s.trial, s.arm)).collect();
    assert_eq!(
        order,
        [
            (1, Arm::Base),
            (1, Arm::Candidate),
            (2, Arm::Candidate),
            (2, Arm::Base)
        ]
    );

    let report = fs::read_to_string(&page).expect("report");
    assert!(
        report.contains("| Total tokens | 2,000 | 1,600 | -20.0% | -20.0% to -20.0% |"),
        "{report}"
    );
    assert!(
        report.contains("Passed: base 2/2, candidate 2/2. 2 of 2 pairs measured."),
        "{report}"
    );
    assert!(report.contains("Base: `bake 0.0.0-standin`"), "{report}");
    assert!(!report.contains("same executable"), "{report}");
    let record = fs::read_to_string(scratch.path("out/run.json")).expect("record");
    assert!(record.contains("\"same_executable\": false"), "{record}");

    // Kept sample directories hold the event stream and no account.
    let kept = files_under(&scratch.path("work"));
    assert!(
        kept.iter().any(|path| path.ends_with("stream.jsonl")),
        "{kept:?}"
    );
    for path in &kept {
        let name = path.file_name().expect("name").to_string_lossy();
        assert!(name != "auth.json" && name != "settings.json", "{path:?}");
        if let Ok(text) = fs::read_to_string(path) {
            assert!(!text.contains("secret-key"), "{path:?} holds the key");
        }
    }
    // The source home is untouched.
    let source = fs::read_to_string(scratch.path("source/settings.json")).expect("source");
    assert!(source.contains("telecom"));

    // A second run does not overwrite the records.
    let again = options(
        &scratch,
        scratch.path("base-bake"),
        scratch.path("candidate-bake"),
    );
    let error = run::run(again).await.unwrap_err();
    assert!(
        error.ends_with("already holds a run; choose another directory"),
        "{error}"
    );

    // The report is rewritten from the records alone.
    fs::remove_file(&page).expect("remove");
    assert_eq!(
        run::write_report(&scratch.path("out")).expect("report"),
        page
    );
    assert_eq!(fs::read_to_string(&page).expect("report"), report);
}

#[tokio::test(flavor = "multi_thread")]
async fn one_executable_for_both_arms_is_a_noise_floor() {
    let scratch = Scratch::new("same");
    let bake = scratch.script("bake", &working_bake(500));
    // One file by two spellings, one of them through `..`: a sample runs in
    // its own workspace, so the runner must resolve each before it starts.
    scratch.tasks();
    let spelled = scratch.path("tasks/../bake");
    let mut options = options(&scratch, spelled, bake);
    options.trials = 1;
    options.work = None;
    options.keep_work = false;
    let page = run::run(options).await.expect("run");
    let report = fs::read_to_string(page).expect("report");
    assert!(
        report.contains("**Both arms are the same executable.**"),
        "{report}"
    );
    assert!(
        report.contains("| Total tokens | 500 | 500 | +0.0% | +0.0% to +0.0% |"),
        "{report}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn one_executable_with_two_settings_measures_the_setting() {
    let scratch = Scratch::new("settings");
    // A stand-in that uses fewer tokens when the arm's setting reached its
    // home.
    let script = r#"#!/bin/sh
if [ "$1" = "--version" ]; then echo "bake 0.0.0-standin"; exit 0; fi
tokens=1000
grep -q '"lean":true' "$BAKE_HOME/settings.json" && tokens=800
: > done.txt
echo '{"type":"result","subtype":"success","result":"hello","modelCalls":1,"usage":{"totalTokens":'"$tokens"'}}'
"#;
    let bake = scratch.script("bake", script);
    let mut options = options(&scratch, bake.clone(), bake);
    options.trials = 1;
    options.candidate_settings = Some(scratch.write("lean.json", r#"{"lean":true}"#));
    let page = run::run(options).await.expect("run");
    let report = fs::read_to_string(page).expect("report");
    assert!(
        report.contains("base `{}`, candidate `{\"lean\":true}`"),
        "{report}"
    );
    assert!(
        report.contains("**Both arms are the same executable; they differ in their settings.**"),
        "{report}"
    );
    assert!(
        report.contains("| Total tokens | 1,000 | 800 | -20.0% | -20.0% to -20.0% |"),
        "{report}"
    );
}

fn context(scratch: &Scratch, after_run_limit: Duration) -> Context {
    Context {
        work: scratch.path("work"),
        source_home: scratch.source_home(),
        provider: "cliproxyapi".into(),
        max_model_calls: 9,
        run_timeout_ms: 100,
        after_run_limit,
        keep_work: false,
        base_settings: serde_json::Map::new(),
        candidate_settings: serde_json::Map::new(),
    }
}

async fn one_sample(scratch: &Scratch, script: &str, after_run_limit: Duration) -> Sample {
    let tasks = task::load_tasks(&scratch.tasks(), &[]).expect("tasks");
    let spec = SampleSpec {
        task: Arc::new(tasks[0].clone()),
        model: ModelSpec::parse("m1", "cliproxyapi").expect("model"),
        arm: Arm::Base,
        trial: 1,
        exe: scratch.script("bake", script),
    };
    let sample = sample::run_sample(&spec, &context(scratch, after_run_limit)).await;
    // The sample's directory, and the account in it, are gone.
    assert_eq!(
        fs::read_dir(scratch.path("work"))
            .map(|dir| dir.count())
            .unwrap_or(0),
        0
    );
    sample
}

#[tokio::test(flavor = "multi_thread")]
async fn a_run_that_never_ends_is_stopped_with_its_children() {
    let scratch = Scratch::new("hang");
    let marker = scratch.path("child-survived");
    let script = format!(
        "#!/bin/sh\n(sleep 2; : > {}) &\nsleep 600\n",
        marker.display()
    );
    let started = Instant::now();
    let sample = one_sample(&scratch, &script, Duration::from_millis(300)).await;
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(
        (sample.outcome.as_str(), sample.passed, sample.measured),
        ("timed_out", false, false)
    );
    // The background child was killed with the run: it never writes.
    tokio::time::sleep(Duration::from_millis(2300)).await;
    assert!(!marker.exists(), "a process the run started outlived it");
}

#[tokio::test(flavor = "multi_thread")]
async fn runs_without_a_result_and_failed_checks_are_recorded() {
    let scratch = Scratch::new("fail");
    let limit = Duration::from_secs(20);
    let crashed = one_sample(
        &scratch,
        "#!/bin/sh\necho 'no usable model' >&2\nexit 1\n",
        limit,
    )
    .await;
    assert_eq!(
        (crashed.outcome.as_str(), crashed.measured),
        ("no_result", false)
    );
    assert_eq!(crashed.error.as_deref(), Some("no usable model"));

    // A run that reports success without doing the task fails its check and
    // still counts its tokens.
    let idle = "#!/bin/sh\necho '{\"type\":\"result\",\"subtype\":\"success\",\"result\":\"hello\",\"modelCalls\":1,\"usage\":{\"totalTokens\":40}}'\n";
    let idle = one_sample(&scratch, idle, limit).await;
    assert_eq!(
        (idle.outcome.as_str(), idle.passed, idle.measured),
        ("success", false, true)
    );
    assert_eq!(
        idle.error.as_deref(),
        Some("check.sh exited with 1: done.txt is missing")
    );
    assert_eq!(idle.total_tokens, 40);

    // A run that ends in an error is not checked.
    let failed = "#!/bin/sh\n: > done.txt\necho '{\"type\":\"result\",\"subtype\":\"budget_exhausted\",\"result\":\"hello\",\"error\":\"out of calls\",\"modelCalls\":9}'\nexit 1\n";
    let failed = one_sample(&scratch, failed, limit).await;
    assert_eq!(
        (failed.outcome.as_str(), failed.passed, failed.measured),
        ("budget_exhausted", false, true)
    );
    assert_eq!(failed.error.as_deref(), Some("out of calls"));

    let missing = SampleSpec {
        task: Arc::new(task::load_tasks(&scratch.tasks(), &[]).expect("tasks")[0].clone()),
        model: ModelSpec::parse("m1", "cliproxyapi").expect("model"),
        arm: Arm::Candidate,
        trial: 1,
        exe: scratch.path("no-such-bake"),
    };
    let missing = sample::run_sample(&missing, &context(&scratch, limit)).await;
    assert_eq!(missing.outcome, "start_failed");
}

#[tokio::test(flavor = "multi_thread")]
async fn task_checks_are_proved_without_a_model() {
    let scratch = Scratch::new("tasks");
    let dir = scratch.tasks();
    let tasks = task::load_tasks(&dir, &[]).expect("tasks");
    assert_eq!(
        run::check_tasks(&tasks).await.expect("check"),
        Vec::<String>::new()
    );

    // A check that cannot fail, one that cannot pass, and a missing solution.
    for (name, check) in [("lax", "exit 0\n"), ("strict", "echo never; exit 1\n")] {
        scratch.write(&format!("tasks/{name}/prompt.md"), "Do it.");
        scratch.write(&format!("tasks/{name}/workspace/a.txt"), "");
        scratch.write(&format!("tasks/{name}/check.sh"), check);
        scratch.write(&format!("tasks/{name}/solution/a.txt"), "");
    }
    scratch.write("tasks/bare/prompt.md", "Do it.");
    scratch.write("tasks/bare/workspace/a.txt", "");
    scratch.write("tasks/bare/check.sh", "exit 1\n");
    let tasks = task::load_tasks(&dir, &[]).expect("tasks");
    assert_eq!(
        run::check_tasks(&tasks).await.expect("check"),
        [
            "bare: no solution directory",
            "lax: check.sh passes on the untouched workspace",
            "strict: check.sh fails on the solution: check.sh exited with 1: never",
        ]
    );
}

/// The tasks shipped with the crate, which need `python3`.
#[tokio::test(flavor = "multi_thread")]
async fn the_shipped_tasks_prove_their_checks() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tasks");
    let tasks = task::load_tasks(&dir, &[]).expect("tasks");
    assert_eq!(
        tasks
            .iter()
            .map(|task| task.name.as_str())
            .collect::<Vec<_>>(),
        [
            "add-flag",
            "cascade-fix",
            "fix-test",
            "hanging-command",
            "hidden-cause-bug",
            "large-output",
            "log-count",
            "multi-module-refactor",
            "nested-instructions",
            "rename-api",
            "skill-crowd",
            "skill-procedure",
            "trace-question"
        ]
    );
    assert_eq!(
        run::check_tasks(&tasks).await.expect("check"),
        Vec::<String>::new(),
        "python3 must be on PATH for the shipped tasks"
    );
}
