//! `bake update`: the command line over [`bake_update`].
//!
//! `bake update` installs the published release and makes it current,
//! `--check` only reports one, `--force <version>` installs the published
//! version even when it is not newer, and `--rollback` returns to the
//! release the last update replaced. Every run takes the install lock and
//! needs a native install: this executable's own root, or the one
//! `BAKE_NATIVE_ROOT` names. Bake 0.4.0 publishes native updates for Linux
//! alone, so on macOS and Windows every run says that those arrive in 0.4.x.

use std::path::PathBuf;
use std::process::ExitCode;

use bake_update::UpdateError;
use bake_update::layout::{Root, detect_root};
use bake_update::manifest::{host_target, release_target};
use bake_update::source::Source;
use bake_update::update::{Outcome, Updater};

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
