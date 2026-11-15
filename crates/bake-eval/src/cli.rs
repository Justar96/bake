//! The `bake-eval` command line.

use std::path::PathBuf;

use crate::run::{self, RunOptions};
use crate::sample::ModelSpec;
use crate::task;

/// The help text.
pub const USAGE: &str = "\
bake-eval: paired evaluation of two bake executables

Usage:
  bake-eval run --base <bake> --candidate <bake> --models <id[:level],...> --out <dir> [options]
  bake-eval report --out <dir>
  bake-eval check-tasks [--tasks <dir>] [--only <task,...>]

run options:
  --name <name>            the record's name; the last part of --out when not given
  --tasks <dir>            the task directory; this crate's tasks/ when not given
  --only <task,...>        run only these tasks
  --trials <n>             trials of each task and model by each arm (3)
  --jobs <n>               samples run at once (4)
  --provider <id>          the provider of models named without one (cliproxyapi)
  --home <dir>             the Bake home whose account is copied; BAKE_HOME, else ~/.bake
  --max-model-calls <n>    each run's request limit (60)
  --run-timeout-ms <n>     each run's time limit (900000)
  --work <dir>             where sample directories are made; a temporary directory when not given
  --keep-work              keep sample directories, with each run's event stream and without its account
  --base-settings <file>   a JSON object of settings.json members for the base arm's samples
  --candidate-settings <file>  the same for the candidate arm's samples

run writes run.json, samples.jsonl, and README.md into --out. report writes README.md again
from the other two. check-tasks proves every task's check with its solution and no model.";

/// The tasks shipped with this crate.
fn default_tasks() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tasks")
}

fn default_home() -> Result<PathBuf, String> {
    if let Some(home) = std::env::var_os("BAKE_HOME").filter(|home| !home.is_empty()) {
        return Ok(PathBuf::from(home));
    }
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .filter(|home| !home.is_empty())
        .map(|home| PathBuf::from(home).join(".bake"))
        .ok_or_else(|| "no home directory; give --home".to_owned())
}

/// The flags of one command: each name once, with its value unless it is a
/// switch.
struct Flags {
    values: Vec<(String, Option<String>)>,
}

impl Flags {
    fn parse(args: &[String], valued: &[&str], switches: &[&str]) -> Result<Self, String> {
        let mut values: Vec<(String, Option<String>)> = Vec::new();
        let mut rest = args.iter();
        while let Some(arg) = rest.next() {
            let value = if valued.contains(&arg.as_str()) {
                Some(
                    rest.next()
                        .ok_or_else(|| format!("{arg} needs a value"))?
                        .clone(),
                )
            } else if switches.contains(&arg.as_str()) {
                None
            } else {
                return Err(format!("unknown argument {arg:?}"));
            };
            if values.iter().any(|(name, _)| name == arg) {
                return Err(format!("{arg} is given twice"));
            }
            values.push((arg.clone(), value));
        }
        Ok(Self { values })
    }

    fn value(&self, name: &str) -> Option<&str> {
        self.values
            .iter()
            .find(|(known, _)| known == name)
            .and_then(|(_, value)| value.as_deref())
    }

    fn has(&self, name: &str) -> bool {
        self.values.iter().any(|(known, _)| known == name)
    }

    fn required(&self, name: &str) -> Result<&str, String> {
        self.value(name)
            .ok_or_else(|| format!("{name} is required"))
    }

    fn number<T: std::str::FromStr>(&self, name: &str, default: T) -> Result<T, String> {
        match self.value(name) {
            Some(text) => text
                .parse()
                .map_err(|_| format!("{name} needs a whole number, not {text:?}")),
            None => Ok(default),
        }
    }

    fn list(&self, name: &str) -> Vec<String> {
        self.value(name)
            .map(|text| {
                text.split(',')
                    .map(str::trim)
                    .filter(|part| !part.is_empty())
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// The options of `bake-eval run <args>`.
pub fn run_options(args: &[String]) -> Result<RunOptions, String> {
    let flags = Flags::parse(
        args,
        &[
            "--base",
            "--candidate",
            "--models",
            "--out",
            "--name",
            "--tasks",
            "--only",
            "--trials",
            "--jobs",
            "--provider",
            "--home",
            "--max-model-calls",
            "--run-timeout-ms",
            "--work",
            "--base-settings",
            "--candidate-settings",
        ],
        &["--keep-work"],
    )?;
    let provider = flags
        .value("--provider")
        .unwrap_or("cliproxyapi")
        .to_owned();
    let models = flags
        .list("--models")
        .iter()
        .map(|model| ModelSpec::parse(model, &provider))
        .collect::<Result<Vec<_>, _>>()?;
    if models.is_empty() {
        return Err("--models is required".to_owned());
    }
    let out = PathBuf::from(flags.required("--out")?);
    let name = match flags.value("--name") {
        Some(name) => name.to_owned(),
        None => out
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .ok_or_else(|| "--out has no last part to name the record; give --name".to_owned())?,
    };
    Ok(RunOptions {
        name,
        base: PathBuf::from(flags.required("--base")?),
        candidate: PathBuf::from(flags.required("--candidate")?),
        models,
        tasks: flags
            .value("--tasks")
            .map_or_else(default_tasks, PathBuf::from),
        only: flags.list("--only"),
        trials: flags.number("--trials", 3)?,
        jobs: flags.number("--jobs", 4)?,
        source_home: match flags.value("--home") {
            Some(home) => PathBuf::from(home),
            None => default_home()?,
        },
        provider,
        max_model_calls: flags.number("--max-model-calls", 60)?,
        run_timeout_ms: flags.number("--run-timeout-ms", 900_000)?,
        out,
        work: flags.value("--work").map(PathBuf::from),
        keep_work: flags.has("--keep-work"),
        base_settings: flags.value("--base-settings").map(PathBuf::from),
        candidate_settings: flags.value("--candidate-settings").map(PathBuf::from),
    })
}

fn runtime() -> Result<tokio::runtime::Runtime, String> {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("cannot start the runtime: {error}"))
}

/// Runs the command line `args`, without the program name.
pub fn main(args: Vec<String>) -> Result<(), String> {
    let Some((command, rest)) = args.split_first() else {
        return Err(format!("no command\n\n{USAGE}"));
    };
    match command.as_str() {
        "run" => {
            let options = run_options(rest)?;
            let page = runtime()?.block_on(run::run(options))?;
            println!("{}", page.display());
            Ok(())
        }
        "report" => {
            let flags = Flags::parse(rest, &["--out"], &[])?;
            let page = run::write_report(&PathBuf::from(flags.required("--out")?))?;
            println!("{}", page.display());
            Ok(())
        }
        "check-tasks" => {
            let flags = Flags::parse(rest, &["--tasks", "--only"], &[])?;
            let dir = flags
                .value("--tasks")
                .map_or_else(default_tasks, PathBuf::from);
            let tasks = task::load_tasks(&dir, &flags.list("--only"))?;
            let problems = runtime()?.block_on(run::check_tasks(&tasks))?;
            if problems.is_empty() {
                println!(
                    "{} tasks: every check fails untouched and passes on its solution",
                    tasks.len()
                );
                Ok(())
            } else {
                Err(problems.join("\n"))
            }
        }
        "help" | "--help" | "-h" => {
            println!("{USAGE}");
            Ok(())
        }
        other => Err(format!("unknown command {other:?}\n\n{USAGE}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(text: &str) -> Vec<String> {
        text.split_whitespace().map(str::to_owned).collect()
    }

    #[test]
    fn run_options_take_defaults_and_flags() {
        let options = run_options(&args(
            "--base b --candidate c --models x:high,other/y --out records/2026-01-01-name --home h",
        ))
        .expect("options");
        assert_eq!(options.name, "2026-01-01-name");
        assert_eq!(
            options
                .models
                .iter()
                .map(ModelSpec::label)
                .collect::<Vec<_>>(),
            ["cliproxyapi/x:high", "other/y"]
        );
        assert_eq!((options.trials, options.jobs), (3, 4));
        assert_eq!(
            (options.max_model_calls, options.run_timeout_ms),
            (60, 900_000)
        );
        assert_eq!(options.tasks, default_tasks());
        assert!(!options.keep_work && options.work.is_none() && options.only.is_empty());

        let options = run_options(&args(
            "--base b --candidate c --models x --out o --home h --provider gw --name n --trials 5 --jobs 2 --only a,b --keep-work --work w --max-model-calls 7 --run-timeout-ms 9 --tasks t",
        ))
        .expect("options");
        assert_eq!(options.models[0].label(), "gw/x");
        assert_eq!(
            (options.name.as_str(), options.trials, options.jobs),
            ("n", 5, 2)
        );
        assert_eq!(options.only, ["a", "b"]);
        assert!(options.keep_work);
        assert_eq!(options.work, Some(PathBuf::from("w")));
        assert_eq!((options.max_model_calls, options.run_timeout_ms), (7, 9));
        assert_eq!(options.tasks, PathBuf::from("t"));
    }

    #[test]
    fn bad_arguments_are_named() {
        let error = |text: &str| run_options(&args(text)).unwrap_err();
        assert_eq!(
            error("--base b --candidate c --out o --home h"),
            "--models is required"
        );
        assert_eq!(
            error("--models x --candidate c --out o --home h"),
            "--base is required"
        );
        assert_eq!(error("--models x --base"), "--base needs a value");
        assert_eq!(error("--models x --models y"), "--models is given twice");
        assert_eq!(error("--frobnicate"), "unknown argument \"--frobnicate\"");
        assert_eq!(
            error("--base b --candidate c --models x --out o --home h --trials many"),
            "--trials needs a whole number, not \"many\""
        );
        assert!(main(Vec::new()).unwrap_err().starts_with("no command"));
        assert!(
            main(args("frob"))
                .unwrap_err()
                .starts_with("unknown command \"frob\"")
        );
    }
}
