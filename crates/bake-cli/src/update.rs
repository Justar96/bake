//! `bake update`: the command line over [`bake_update`].
//!
//! `bake update` installs the published release and makes it current,
//! `--check` only reports one, `--force <version>` installs the published
//! version even when it is not newer, and `--rollback` returns to the
//! release the last update replaced. Every run takes the install lock and
//! needs a native install: this executable's own root, or the one
//! `BAKE_NATIVE_ROOT` names. Bake 0.4.0 publishes native updates for Linux
//! alone, so on macOS and Windows every run says that those arrive in 0.4.x.
//!
//! An interactive session on such an install also checks for a published
//! release when it starts ([`startup_check`]) and says when one is newer;
//! it remembers the answer in the Bake home for a day, so most launches
//! reach no network for it.

use std::cmp::Ordering;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use bake_update::UpdateError;
use bake_update::layout::{Root, detect_root};
use bake_update::manifest::{host_target, release_target};
use bake_update::source::Source;
use bake_update::update::{Outcome, Updater};
use bake_update::version::{compare_versions, is_release_version};
use serde_json::{Value, json};

/// What `bake update` was asked to do.
#[derive(Debug, PartialEq, Eq)]
pub enum Request {
    /// Report a published update without installing it.
    Check,
    /// Install the published release; `Some(version)` is `--force`.
    Install(Option<String>),
    /// Return to the release the last update replaced.
    Rollback,
}

/// The status `--check` exits with when a newer release is published, so
/// a script can tell "an update is waiting" from "the check failed".
pub const UPDATE_AVAILABLE: u8 = 10;

/// Read `bake update`'s arguments.
///
/// # Errors
///
/// The reason the arguments are not a run, as `main`'s usage errors word
/// it.
pub fn parse(args: &[&str]) -> Result<Request, String> {
    let mut check = false;
    let mut rollback = false;
    let mut force: Option<String> = None;
    let mut rest = args.iter();
    while let Some(argument) = rest.next() {
        match *argument {
            "--check" => check = true,
            "--rollback" => rollback = true,
            "--force" => {
                let Some(version) = rest.next() else {
                    return Err("'update --force' needs the version to install".to_owned());
                };
                force = Some((*version).to_owned());
            }
            other => {
                return Err(format!("unexpected argument '{other}' for 'update'"));
            }
        }
    }
    match (check, rollback, force) {
        (true, false, None) => Ok(Request::Check),
        (false, true, None) => Ok(Request::Rollback),
        (false, false, force) => Ok(Request::Install(force)),
        _ => Err(
            "'update' takes --check, --force <version>, or --rollback, not more than one of them"
                .to_owned(),
        ),
    }
}

/// Run one request against this host's native install.
pub fn run(request: &Request) -> ExitCode {
    let root = match install_root() {
        Ok(root) => root,
        Err(message) => return fail(&message),
    };
    let updater = match Updater::new(Root::new(root), Source::default(), crate::VERSION) {
        Ok(updater) => updater,
        Err(error) => return fail(error.message()),
    };
    let outcome = match request {
        Request::Rollback => updater.rollback(),
        Request::Check | Request::Install(_) => {
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(error) => return fail(&format!("could not start the async runtime: {error}")),
            };
            runtime.block_on(async {
                match request {
                    Request::Check => updater.check_only().await,
                    Request::Install(force) => updater.update(force.as_deref()).await,
                    // The rollback is answered above; it reaches no network.
                    Request::Rollback => unreachable!(),
                }
            })
        }
    };
    match outcome {
        Ok(outcome) => report(request, &outcome),
        Err(error) => fail(error.message()),
    }
}

/// The file in the Bake home that remembers the last startup check.
const CHECK_FILE: &str = "update-check.json";

/// How long a startup check's answer stands before a launch fetches the
/// manifest again.
const CHECK_EVERY: Duration = Duration::from_secs(24 * 60 * 60);

/// The interactive startup's update check, or `None` when this `bake`
/// runs from no native install, such as a source build, or on a platform
/// Bake publishes no native release for. The check reads the answer
/// `home` remembers when it is under a day old, and otherwise fetches the
/// signed manifest as `bake update --check` does; it installs nothing and
/// takes no install lock. Any failure is silent: it yields no notice.
pub fn startup_check(home: &Path) -> Option<bake_tui::UpdateCheck> {
    let root = install_root().ok()?;
    let updater = Updater::new(Root::new(root), Source::default(), crate::VERSION).ok()?;
    let cache = home.join(CHECK_FILE);
    Some(Box::pin(async move {
        let installed = updater.installed();
        let fetch = async {
            match updater.check_only().await.ok()? {
                Outcome::Available { version, .. } | Outcome::UpToDate { version } => Some(version),
                _ => None,
            }
        };
        let latest = latest_release(&cache, SystemTime::now(), fetch).await?;
        update_notice(&latest, &installed)
    }))
}

/// The newest published version: the one `cache` remembers when it was
/// checked less than [`CHECK_EVERY`] before `now`, or what `fetch` finds,
/// which `cache` then remembers.
async fn latest_release(
    cache: &Path,
    now: SystemTime,
    fetch: impl Future<Output = Option<String>>,
) -> Option<String> {
    let now = now.duration_since(UNIX_EPOCH).ok()?.as_secs();
    if let Some(latest) = std::fs::read_to_string(cache)
        .ok()
        .and_then(|text| remembered(&text, now))
    {
        return Some(latest);
    }
    let latest = fetch.await?;
    remember(cache, &latest, now);
    Some(latest)
}

/// The version a check file names, when it is a release version checked
/// less than [`CHECK_EVERY`] before `now`; a check dated in the future is
/// stale, so a clock set back does not hold an answer.
fn remembered(text: &str, now: u64) -> Option<String> {
    let value: Value = serde_json::from_str(text).ok()?;
    let checked = value.get("checked")?.as_u64()?;
    let latest = value.get("latest")?.as_str()?;
    let fresh = checked <= now && now - checked < CHECK_EVERY.as_secs();
    (fresh && is_release_version(latest)).then(|| latest.to_owned())
}

/// Writes the check file through a temporary file and a rename, so a
/// concurrent launch reads the old answer or the new one, never part of
/// one. A failed write only means the next launch checks again.
fn remember(cache: &Path, latest: &str, now: u64) {
    let text = format!("{}\n", json!({ "checked": now, "latest": latest }));
    let mut temp = cache.as_os_str().to_owned();
    temp.push(format!(".{}.tmp", std::process::id()));
    let temp = PathBuf::from(temp);
    if std::fs::write(&temp, text)
        .and_then(|()| std::fs::rename(&temp, cache))
        .is_err()
    {
        let _ = std::fs::remove_file(&temp);
    }
}

/// The notice for `latest` when it is newer than `installed`.
fn update_notice(latest: &str, installed: &str) -> Option<bake_tui::Notice> {
    (compare_versions(latest, installed) == Ordering::Greater).then(|| bake_tui::Notice {
        tag: "update".to_owned(),
        text: format!(
            "Bake {latest} is available; this install runs {installed}. Run 'bake update' to install it."
        ),
    })
}

/// Where this install is: `BAKE_NATIVE_ROOT`, or the root this
/// executable runs from.
fn install_root() -> Result<PathBuf, String> {
    let executable = std::env::current_exe().ok();
    detect_root(&|name| std::env::var_os(name), executable.as_deref())
        .ok_or_else(|| no_install(release_target(host_target())))
}

/// Why a run found no native install. On a platform Bake publishes no
/// native release for, the reason is the platform, not a missing install,
/// so it says that instead of sending the user to an installer that refuses.
fn no_install(platform: Result<&'static str, UpdateError>) -> String {
    match platform {
        Ok(_) => "update needs a native install, and this bake was not started from one; \
                  install Bake from https://bake.justar.dev, or set BAKE_NATIVE_ROOT to the install root"
            .to_owned(),
        Err(error) => format!("update needs a native install, and {}", error.message()),
    }
}

/// Print what the run found or did, and exit with its status. A closed
/// standard output is status 1, as it is everywhere else in `bake`.
fn report(request: &Request, outcome: &Outcome) -> ExitCode {
    let (text, code) = message(request, outcome);
    if crate::write_stdout(&text) {
        code
    } else {
        ExitCode::FAILURE
    }
}

/// What one outcome prints, and the status it exits with.
fn message(request: &Request, outcome: &Outcome) -> (String, ExitCode) {
    match outcome {
        Outcome::UpToDate { version } => (
            format!("bake {version} is up to date.\n"),
            ExitCode::SUCCESS,
        ),
        Outcome::Available { installed, version } => (
            format!(
                "Bake {version} is available; this install runs {installed}.\nRun 'bake update' to install it.\n"
            ),
            if matches!(request, Request::Check) {
                ExitCode::from(UPDATE_AVAILABLE)
            } else {
                ExitCode::SUCCESS
            },
        ),
        Outcome::Updated { from, to, pruned } => {
            let mut lines = match from {
                Some(from) => format!(
                    "Bake {to} is installed and current; {from} is kept.\nRun 'bake update --rollback' to return to it.\n"
                ),
                None => format!("Bake {to} is installed and current.\n"),
            };
            if !pruned.is_empty() {
                lines.push_str(&format!("Removed Bake {}.\n", pruned.join(", ")));
            }
            (lines, ExitCode::SUCCESS)
        }
        Outcome::RolledBack { from, to } => (
            format!("Bake {to} is current again; {from} is still installed.\n"),
            ExitCode::SUCCESS,
        ),
        Outcome::NoPrevious { current } => (
            format!(
                "Bake {current} is the only release this install can run; there is nothing to roll back to.\n"
            ),
            ExitCode::SUCCESS,
        ),
    }
}

/// An update failure: the plain reason on standard error, status 1, as
/// [`UpdateError`] words it.
fn fail(message: &str) -> ExitCode {
    let _ = std::io::Write::write_all(
        &mut std::io::stderr().lock(),
        format!("bake: {message}\n").as_bytes(),
    );
    ExitCode::FAILURE
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_arguments_name_one_run() {
        assert_eq!(parse(&[]), Ok(Request::Install(None)));
        assert_eq!(parse(&["--check"]), Ok(Request::Check));
        assert_eq!(parse(&["--rollback"]), Ok(Request::Rollback));
        assert_eq!(
            parse(&["--force", "0.4.1"]),
            Ok(Request::Install(Some("0.4.1".to_owned())))
        );
        assert_eq!(
            parse(&["--force"]),
            Err("'update --force' needs the version to install".to_owned())
        );
        assert_eq!(
            parse(&["--checks"]),
            Err("unexpected argument '--checks' for 'update'".to_owned())
        );
        for args in [
            &["--check", "--rollback"][..],
            &["--check", "--force", "0.4.1"],
            &["--rollback", "--force", "0.4.1"],
        ] {
            assert!(parse(args).is_err(), "{args:?}");
        }
    }

    #[test]
    fn without_an_install_a_platform_outside_the_release_says_when_updates_arrive() {
        assert!(
            no_install(Ok("linux-x64")).contains("install Bake from https://bake.justar.dev"),
            "{}",
            no_install(Ok("linux-x64"))
        );
        assert_eq!(
            no_install(release_target(Some("darwin-arm64"))),
            "update needs a native install, and Bake 0.4.0 publishes native updates for \
             linux-arm64 and linux-x64 only; native updates for darwin-arm64 arrive in Bake 0.4.x"
        );
        assert_eq!(
            no_install(release_target(Some("win32-x64"))),
            "update needs a native install, and Bake 0.4.0 publishes native updates for \
             linux-arm64 and linux-x64 only; native updates for win32-x64 arrive in Bake 0.4.x"
        );
        // With an install root named, the updater itself refuses the platform.
        let root = Root::new(std::env::temp_dir().join("bake-update-platform-unused"));
        match Updater::new(root, Source::default(), "0.4.0") {
            Ok(_) => assert!(release_target(host_target()).is_ok()),
            Err(error) => {
                assert_eq!(Err(error), release_target(host_target()).map(|_| ()));
            }
        }
    }

    #[test]
    fn a_startup_notice_names_only_a_newer_release() {
        assert_eq!(
            update_notice("0.4.3", "0.4.2"),
            Some(bake_tui::Notice {
                tag: "update".to_owned(),
                text: "Bake 0.4.3 is available; this install runs 0.4.2. Run 'bake update' to install it."
                    .to_owned(),
            })
        );
        assert_eq!(update_notice("0.4.2", "0.4.2"), None);
        assert_eq!(update_notice("0.4.1", "0.4.2"), None);
        assert_eq!(update_notice("0.5.0-rc.1", "0.5.0"), None);
    }

    #[test]
    fn a_remembered_check_stands_for_a_day_and_only_names_a_version() {
        let day = CHECK_EVERY.as_secs();
        let file = |checked: u64, latest: &str| {
            json!({ "checked": checked, "latest": latest }).to_string()
        };
        assert_eq!(
            remembered(&file(1000, "0.4.3"), 1000).as_deref(),
            Some("0.4.3")
        );
        assert_eq!(
            remembered(&file(1000, "0.4.3"), 1000 + day - 1).as_deref(),
            Some("0.4.3")
        );
        assert_eq!(remembered(&file(1000, "0.4.3"), 1000 + day), None);
        // Dated in the future, it is stale.
        assert_eq!(remembered(&file(1001, "0.4.3"), 1000), None);
        // Text that is not a release version, or not a check file, is ignored.
        for text in [
            file(1000, "0.4.3\u{1b}[2J"),
            file(1000, "v0.4.3"),
            file(1000, ""),
            "{\"checked\": -1, \"latest\": \"0.4.3\"}".to_owned(),
            "{\"latest\": \"0.4.3\"}".to_owned(),
            "[]".to_owned(),
            "not json".to_owned(),
            String::new(),
        ] {
            assert_eq!(remembered(&text, 1000), None, "{text}");
        }
    }

    #[test]
    fn a_startup_check_fetches_once_a_day_and_remembers_the_answer() {
        let dir = std::env::temp_dir().join(format!(
            "bake-update-startup-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |time| time.as_nanos())
        ));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let cache = dir.join(CHECK_FILE);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("runtime");
        let at = |secs: u64| UNIX_EPOCH + Duration::from_secs(secs);
        let fetches = std::cell::Cell::new(0);
        let check = |now, answer: Option<&str>| {
            runtime.block_on(latest_release(&cache, now, async {
                fetches.set(fetches.get() + 1);
                answer.map(str::to_owned)
            }))
        };
        // Nothing remembered: it fetches, and remembers.
        assert_eq!(check(at(1000), Some("0.4.3")).as_deref(), Some("0.4.3"));
        assert_eq!(fetches.get(), 1);
        // Within the day the remembered answer stands.
        assert_eq!(check(at(2000), Some("0.4.4")).as_deref(), Some("0.4.3"));
        assert_eq!(fetches.get(), 1);
        // A day later it fetches again; a failed fetch keeps the old file.
        let later = 1000 + CHECK_EVERY.as_secs();
        assert_eq!(check(at(later), None), None);
        assert_eq!(fetches.get(), 2);
        assert_eq!(check(at(later), Some("0.4.4")).as_deref(), Some("0.4.4"));
        assert_eq!(fetches.get(), 3);
        assert_eq!(check(at(later + 1), None).as_deref(), Some("0.4.4"));
        // No temporary file is left beside it.
        let names: Vec<_> = std::fs::read_dir(&dir)
            .expect("read dir")
            .map(|entry| entry.expect("entry").file_name())
            .collect();
        assert_eq!(names, [std::ffi::OsString::from(CHECK_FILE)]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_waiting_update_is_status_10_only_for_a_check() {
        assert_eq!(UPDATE_AVAILABLE, 10);
        let available = Outcome::Available {
            installed: "0.4.0".to_owned(),
            version: "0.4.1".to_owned(),
        };
        let (text, code) = message(&Request::Check, &available);
        assert!(text.contains("Bake 0.4.1 is available"), "{text}");
        assert_eq!(code, ExitCode::from(UPDATE_AVAILABLE));
        let (_, code) = message(&Request::Install(None), &available);
        assert_eq!(code, ExitCode::SUCCESS);
        let (text, code) = message(
            &Request::Install(None),
            &Outcome::Updated {
                from: Some("0.4.0".to_owned()),
                to: "0.4.1".to_owned(),
                pruned: vec!["0.3.9".to_owned()],
            },
        );
        assert!(
            text.contains("Bake 0.4.1 is installed and current"),
            "{text}"
        );
        assert!(text.contains("Removed Bake 0.3.9."), "{text}");
        assert_eq!(code, ExitCode::SUCCESS);
        let (text, _) = message(
            &Request::Rollback,
            &Outcome::RolledBack {
                from: "0.4.1".to_owned(),
                to: "0.4.0".to_owned(),
            },
        );
        assert_eq!(
            text,
            "Bake 0.4.0 is current again; 0.4.1 is still installed.\n"
        );
    }
}
