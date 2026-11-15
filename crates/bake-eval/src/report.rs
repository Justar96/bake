//! The evaluation record: what was run and what it measured, as Markdown.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use serde::{Deserialize, Serialize};

use crate::sample::{Arm, Sample};
use crate::stats::{self, Change};

/// One arm's executable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Executable {
    /// Its path as given.
    pub path: String,
    /// What `--version` printed.
    pub version: String,
    /// Its size in bytes.
    pub bytes: u64,
}

/// What a run was, `run.json` beside its samples.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunRecord {
    /// The record's name.
    pub name: String,
    /// The UTC date the run started, `YYYY-MM-DD`.
    pub date: String,
    /// The base arm.
    pub base: Executable,
    /// The candidate arm.
    pub candidate: Executable,
    /// Whether both arms are one file; with equal settings too, the run is
    /// a noise floor.
    pub same_executable: bool,
    /// The `settings.json` members of the base arm's samples.
    #[serde(default)]
    pub base_settings: serde_json::Map<String, serde_json::Value>,
    /// The `settings.json` members of the candidate arm's samples.
    #[serde(default)]
    pub candidate_settings: serde_json::Map<String, serde_json::Value>,
    /// The model labels, in the order given.
    pub models: Vec<String>,
    /// The task names with their prompts.
    pub tasks: Vec<(String, String)>,
    /// Trials of each task and model by each arm.
    pub trials: u32,
    /// Samples run at once.
    pub jobs: usize,
    /// `--max-model-calls` of each run.
    pub max_model_calls: u64,
    /// `--run-timeout-ms` of each run.
    pub run_timeout_ms: u64,
    /// The host, as `os/arch`.
    pub host: String,
}

/// The UTC calendar date of `seconds` after the Unix epoch.
pub fn utc_date(seconds: u64) -> String {
    // Howard Hinnant's `civil_from_days`.
    let days = (seconds / 86_400) as i64 + 719_468;
    let era = days.div_euclid(146_097);
    let day_of_era = days.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}

fn percent(value: f64) -> String {
    format!("{:+.1}%", value * 100.0)
}

/// A whole number with thousands separators.
fn grouped(value: f64) -> String {
    // An empty sum is -0.0, which would print as "-0".
    let digits = format!("{:.0}", value.round() + 0.0);
    let mut out = String::new();
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

type PairKey<'a> = (&'a str, u32);

/// A row of a model's table: its name, its measure of a sample, and the
/// divisor that brings the measure to the row's unit.
type MeasureRow = (&'static str, fn(&Sample) -> f64, f64);

/// The pairs of one model whose two samples were both measured.
fn pairs<'a>(samples: &'a [Sample], model: &str) -> Vec<(&'a Sample, &'a Sample)> {
    let mut by_key: BTreeMap<PairKey<'a>, (Option<&'a Sample>, Option<&'a Sample>)> =
        BTreeMap::new();
    for sample in samples
        .iter()
        .filter(|sample| sample.model == model && sample.measured)
    {
        let slot = by_key
            .entry((sample.task.as_str(), sample.trial))
            .or_default();
        match sample.arm {
            Arm::Base => slot.0 = Some(sample),
            Arm::Candidate => slot.1 = Some(sample),
        }
    }
    by_key
        .into_values()
        .filter_map(|(base, candidate)| Some((base?, candidate?)))
        .collect()
}

fn change_of(pairs: &[(&Sample, &Sample)], measure: impl Fn(&Sample) -> f64) -> Change {
    let values: Vec<(f64, f64)> = pairs
        .iter()
        .map(|(base, candidate)| (measure(base), measure(candidate)))
        .collect();
    stats::paired_change(&values)
}

/// The sentence under a model's table: its requests and tokens with every
/// task weighing the same.
fn typical_line(out: &mut String, pairs: &[(&Sample, &Sample)]) {
    let of = |measure: fn(&Sample) -> f64| {
        let mut by_task: BTreeMap<&str, Vec<(f64, f64)>> = BTreeMap::new();
        for (base, candidate) in pairs {
            by_task
                .entry(base.task.as_str())
                .or_default()
                .push((measure(base), measure(candidate)));
        }
        let groups: Vec<Vec<(f64, f64)>> = by_task.into_values().collect();
        let typical = stats::typical_change(&groups);
        match (typical.change, typical.interval) {
            (Some(change), Some((low, high))) => {
                format!(
                    "{} ({} to {})",
                    percent(change),
                    percent(low),
                    percent(high)
                )
            }
            (Some(change), None) => percent(change),
            (None, _) => "n/a".to_owned(),
        }
    };
    let _ = writeln!(
        out,
        "\nWith every task weighing the same: model requests {}, total tokens {}.",
        of(|sample| sample.model_calls as f64),
        of(|sample| sample.total_tokens as f64),
    );
}

fn measure_row(out: &mut String, name: &str, change: Change, scale: f64) {
    let relative = change.change.map_or_else(|| "n/a".to_owned(), percent);
    let interval = change.interval.map_or_else(
        || "n/a".to_owned(),
        |(low, high)| format!("{} to {}", percent(low), percent(high)),
    );
    let _ = writeln!(
        out,
        "| {name} | {} | {} | {relative} | {interval} |",
        grouped(change.base / scale),
        grouped(change.candidate / scale)
    );
}

fn passes(samples: &[Sample], model: &str, task: Option<&str>, arm: Arm) -> String {
    let of: Vec<&Sample> = samples
        .iter()
        .filter(|sample| {
            sample.model == model
                && sample.arm == arm
                && task.is_none_or(|task| sample.task == task)
        })
        .collect();
    format!(
        "{}/{}",
        of.iter().filter(|sample| sample.passed).count(),
        of.len()
    )
}

fn mean_tokens(samples: &[Sample], model: &str, task: &str, arm: Arm) -> String {
    let tokens: Vec<f64> = samples
        .iter()
        .filter(|sample| {
            sample.model == model && sample.task == task && sample.arm == arm && sample.measured
        })
        .map(|sample| sample.total_tokens as f64)
        .collect();
    if tokens.is_empty() {
        "n/a".to_owned()
    } else {
        grouped(tokens.iter().sum::<f64>() / tokens.len() as f64)
    }
}

/// One line of text for a table cell or list item.
fn one_line(text: &str, max: usize) -> String {
    let flat = text
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .replace('|', "\\|");
    if flat.chars().count() <= max {
        flat
    } else {
        format!("{}…", flat.chars().take(max).collect::<String>())
    }
}

/// The record of `run` and its `samples`.
pub fn render(run: &RunRecord, samples: &[Sample]) -> String {
    let mut out = String::new();
    let measured = samples.iter().filter(|sample| sample.measured).count();
    let _ = writeln!(out, "# Evaluation: {}\n", run.name);
    let _ = writeln!(
        out,
        "Run on {} on {}. Base: `{}` (`{}`, {} bytes). Candidate: `{}` (`{}`, {} bytes). {} tasks, {} models, and {} trials of each by each arm: {} samples, of which {} printed a result. Each run had `--max-model-calls {}` and `--run-timeout-ms {}`, and {} samples ran at once.\n",
        run.date,
        run.host,
        run.base.version,
        run.base.path,
        run.base.bytes,
        run.candidate.version,
        run.candidate.path,
        run.candidate.bytes,
        run.tasks.len(),
        run.models.len(),
        run.trials,
        samples.len(),
        measured,
        run.max_model_calls,
        run.run_timeout_ms,
        run.jobs,
    );
    let settings_differ = run.base_settings != run.candidate_settings;
    if !run.base_settings.is_empty() || !run.candidate_settings.is_empty() {
        let shown = |settings: &serde_json::Map<String, serde_json::Value>| {
            serde_json::to_string(settings).unwrap_or_default()
        };
        let _ = writeln!(
            out,
            "Settings of the samples, besides the provider account: base `{}`, candidate `{}`.\n",
            shown(&run.base_settings),
            shown(&run.candidate_settings)
        );
    }
    if run.same_executable && settings_differ {
        out.push_str("**Both arms are the same executable; they differ in their settings.**\n\n");
    } else if run.same_executable {
        out.push_str("**Both arms are the same executable.** This run measures no change: its intervals are the noise floor of this task set, these models, and this trial count. A later change inside them cannot be told from noise.\n\n");
    }
    out.push_str("## Method\n\n");
    out.push_str("`bake-eval` ran every sample as `bake -p --output-format stream-json` in a copy of the task's workspace, with a home directory of its own that held only the provider account and the arm's settings named above, if any, so no user setting, skill, or context file was read. A sample passes when the run ends in `success` and the task's check script exits 0. A base and a candidate sample of the same task, model, and trial number are a pair; the pair's two samples started next to each other. A change is the ratio of the sums over the pairs in which both samples printed a result, and its 95% interval is a percentile bootstrap over those pairs (2,000 resamples, fixed seed). A measure that is 0 in most pairs has no interval. Because one large task can decide a ratio of sums, each model's requests and tokens are also given with every task weighing the same: the geometric mean over the tasks of each task's ratio of sums, resampled within each task. The sample records are in `samples.jsonl` and the run's parameters in `run.json`; `bake-eval report` writes this page from them.\n\n");
    out.push_str("## Results\n\n");
    for model in &run.models {
        let pairs = pairs(samples, model);
        let expected = run.tasks.len() * run.trials as usize;
        let _ = writeln!(out, "### {model}\n");
        let _ = writeln!(
            out,
            "Passed: base {}, candidate {}. {} of {expected} pairs measured.\n",
            passes(samples, model, None, Arm::Base),
            passes(samples, model, None, Arm::Candidate),
            pairs.len(),
        );
        out.push_str(
            "| Measure | Base | Candidate | Change | 95% interval |\n|---|---:|---:|---:|---|\n",
        );
        let rows: [MeasureRow; 8] = [
            ("Model requests", |s| s.model_calls as f64, 1.0),
            ("Total tokens", |s| s.total_tokens as f64, 1.0),
            ("Uncached input tokens", |s| s.input as f64, 1.0),
            ("Output tokens", |s| s.output as f64, 1.0),
            ("Tool calls by the model", |s| s.tool_calls as f64, 1.0),
            ("Tool calls inside codemode", |s| s.nested_calls as f64, 1.0),
            ("Failed tool calls", |s| s.tool_errors as f64, 1.0),
            ("Run time, seconds", |s| s.duration_ms as f64, 1000.0),
        ];
        for (name, measure, scale) in rows {
            measure_row(&mut out, name, change_of(&pairs, measure), scale);
        }
        typical_line(&mut out, &pairs);
        let cost = change_of(&pairs, |sample| sample.cost_usd);
        if cost.base > 0.0 || cost.candidate > 0.0 {
            let _ = writeln!(
                out,
                "\nCost: base ${:.2}, candidate ${:.2}.",
                cost.base, cost.candidate
            );
        }
        out.push('\n');
    }
    out.push_str("## Tasks\n\n");
    out.push_str("| Task | Model | Passed, base | Passed, candidate | Mean tokens, base | Mean tokens, candidate |\n|---|---|---:|---:|---:|---:|\n");
    for (task, _) in &run.tasks {
        for model in &run.models {
            let _ = writeln!(
                out,
                "| {task} | {model} | {} | {} | {} | {} |",
                passes(samples, model, Some(task), Arm::Base),
                passes(samples, model, Some(task), Arm::Candidate),
                mean_tokens(samples, model, task, Arm::Base),
                mean_tokens(samples, model, task, Arm::Candidate),
            );
        }
    }
    out.push_str("\n## Samples that did not pass\n\n");
    let failed: Vec<&Sample> = samples.iter().filter(|sample| !sample.passed).collect();
    if failed.is_empty() {
        out.push_str("None.\n");
    }
    for sample in failed {
        let why = sample.error.as_deref().unwrap_or("no reason recorded");
        let _ = writeln!(
            out,
            "- `{}`, {}, {}, trial {}: `{}`. {}",
            sample.task,
            sample.model,
            sample.arm.name(),
            sample.trial,
            sample.outcome,
            one_line(why, 300)
        );
    }
    out.push_str("\n## Prompts\n\n");
    for (task, prompt) in &run.tasks {
        let _ = writeln!(out, "- `{task}`: {}", one_line(prompt, 2000));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_follow_the_calendar() {
        assert_eq!(utc_date(0), "1970-01-01");
        assert_eq!(utc_date(951_782_400), "2000-02-29");
        assert_eq!(utc_date(951_868_799), "2000-02-29");
        assert_eq!(utc_date(951_868_800), "2000-03-01");
        assert_eq!(utc_date(1_794_614_400), "2026-11-14");
        assert_eq!(utc_date(4_102_444_799), "2099-12-31");
    }

    #[test]
    fn numbers_are_grouped() {
        assert_eq!(grouped(0.0), "0");
        assert_eq!(grouped(Vec::<f64>::new().iter().sum()), "0");
        assert_eq!(grouped(999.4), "999");
        assert_eq!(grouped(1000.0), "1,000");
        assert_eq!(grouped(1_234_567.0), "1,234,567");
        assert_eq!(percent(-0.2), "-20.0%");
        assert_eq!(percent(0.0314), "+3.1%");
    }

    fn sample(task: &str, arm: Arm, trial: u32, tokens: u64, passed: bool) -> Sample {
        Sample {
            task: task.into(),
            model: "route/m:high".into(),
            arm,
            trial,
            passed,
            outcome: if passed { "success" } else { "error" }.into(),
            error: (!passed).then(|| "the model\nfailed | badly".to_owned()),
            measured: true,
            model_calls: tokens / 100,
            turns: 1,
            duration_ms: 2_000,
            input: tokens / 2,
            output: tokens / 10,
            cache_read: 0,
            cache_write: 0,
            total_tokens: tokens,
            cost_usd: 0.0,
            tool_calls: 2,
            nested_calls: 0,
            tool_errors: 0,
            tool_responses: 1,
            batched_responses: 1,
        }
    }

    fn record() -> RunRecord {
        let exe = |path: &str| Executable {
            path: path.into(),
            version: "bake 0.0.0".into(),
            bytes: 7,
        };
        RunRecord {
            name: "example".into(),
            date: "2026-11-14".into(),
            base: exe("/base/bake"),
            candidate: exe("/candidate/bake"),
            same_executable: false,
            base_settings: serde_json::Map::new(),
            candidate_settings: serde_json::Map::new(),
            models: vec!["route/m:high".into()],
            tasks: vec![
                ("one".into(), "Do one.".into()),
                ("two".into(), "Do\ntwo.".into()),
            ],
            trials: 2,
            jobs: 3,
            max_model_calls: 40,
            run_timeout_ms: 1000,
            host: "linux/x86_64".into(),
        }
    }

    #[test]
    fn a_record_states_the_paired_change() {
        let mut samples = Vec::new();
        for trial in 1..=2 {
            for task in ["one", "two"] {
                samples.push(sample(task, Arm::Base, trial, 1000, true));
                samples.push(sample(
                    task,
                    Arm::Candidate,
                    trial,
                    800,
                    task == "one" || trial == 1,
                ));
            }
        }
        // An unmeasured candidate leaves its pair out of the sums.
        samples[7].measured = false;
        let page = render(&record(), &samples);
        assert!(page.starts_with("# Evaluation: example\n\nRun on 2026-11-14 on linux/x86_64. Base: `bake 0.0.0` (`/base/bake`, 7 bytes)."), "{page}");
        assert!(
            page.contains("8 samples, of which 7 printed a result."),
            "{page}"
        );
        assert!(
            page.contains("Passed: base 4/4, candidate 3/4. 3 of 4 pairs measured."),
            "{page}"
        );
        assert!(
            page.contains("| Total tokens | 3,000 | 2,400 | -20.0% | -20.0% to -20.0% |"),
            "{page}"
        );
        assert!(
            page.contains("| Failed tool calls | 0 | 0 | n/a | n/a |"),
            "{page}"
        );
        assert!(
            page.contains("| Run time, seconds | 6 | 6 | +0.0% | +0.0% to +0.0% |"),
            "{page}"
        );
        assert!(
            page.contains("| two | route/m:high | 2/2 | 1/2 | 1,000 | 800 |"),
            "{page}"
        );
        assert!(
            page.contains("\nWith every task weighing the same: model requests -20.0% (-20.0% to -20.0%), total tokens -20.0% (-20.0% to -20.0%).\n"),
            "{page}"
        );
        assert!(
            page.contains(
                "- `two`, route/m:high, candidate, trial 2: `error`. the model failed \\| badly"
            ),
            "{page}"
        );
        assert!(page.contains("- `two`: Do two."), "{page}");
        assert!(!page.contains("same executable"), "{page}");
        assert!(!page.contains("Cost:"), "{page}");
        assert!(page.ends_with("\n") && !page.ends_with("\n\n"), "{page}");
    }

    #[test]
    fn a_noise_floor_says_so_and_passes_list_none() {
        let mut run = record();
        run.same_executable = true;
        let samples = vec![
            sample("one", Arm::Base, 1, 1000, true),
            sample("one", Arm::Candidate, 1, 1100, true),
        ];
        let page = render(&run, &samples);
        assert!(
            page.contains("**Both arms are the same executable.**"),
            "{page}"
        );
        assert!(
            page.contains("## Samples that did not pass\n\nNone.\n"),
            "{page}"
        );
        assert!(!page.contains("Settings of the samples"), "{page}");

        // One executable with two settings measures the setting.
        run.candidate_settings
            .insert("nestedContextFiles".into(), true.into());
        let page = render(&run, &samples);
        assert!(
            page.contains("Settings of the samples, besides the provider account: base `{}`, candidate `{\"nestedContextFiles\":true}`.\n\n**Both arms are the same executable; they differ in their settings.**\n\n## Method"),
            "{page}"
        );
        assert!(!page.contains("noise floor"), "{page}");
    }

    #[test]
    fn a_record_written_before_arm_settings_still_reads() {
        let mut value = serde_json::to_value(record()).expect("value");
        let object = value.as_object_mut().expect("object");
        object.remove("base_settings");
        object.remove("candidate_settings");
        let run: RunRecord = serde_json::from_value(value).expect("record");
        assert_eq!(run, record());
    }
}
