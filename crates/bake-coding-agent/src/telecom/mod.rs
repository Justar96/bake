//! Telecom: the Bake sessions working in one repository on one machine
//! form a team. Each takes a callsign, publishes what it works on, and can
//! message, ask, and answer the others; its model gets a brief of the team
//! before each prompt when the team changed.
//!
//! The design follows pi-telecom (`Justar96/pi-telecom` at `b6fe4d8`, MIT;
//! see the crate's `NOTICE`), with its core only — presence, the board,
//! `send`, `ask`, `reply`, `broadcast`, `status`, and the brief — over a
//! protocol of Bake's own instead of pi-telecom's inbox files:
//!
//! - **Membership is a lock** ([`directory`]): a callsign is held by an
//!   exclusive file lock for the process's life, so taking one is atomic
//!   and a member's liveness is whether its lock is held. No heartbeats,
//!   no pid checks, and no renaming races.
//! - **Delivery is a request** ([`endpoint`]): each member listens on a
//!   Unix socket, and a frame is answered `accepted` only once the
//!   recipient's session has it, so a sender always knows who got a
//!   message. There is no inbox to poll and no file left behind.
//! - **The wire is typed and versioned** ([`wire`]): one JSON line per
//!   frame, `v: 1`, a tagged body, and validation of every member before
//!   it reaches a model.
//!
//! Sessions spawned by another ([`crate::spawned`]) are told their
//! parent's callsign and take `<parent>-<n>`, so a parent and its children
//! talk through the same tool.
//!
//! Telecom needs Unix sockets; on Windows a station does not start
//! ([`UNSUPPORTED`]).

pub mod directory;
pub mod endpoint;
pub mod station;
pub mod tool;
pub mod wire;

pub use station::{AskOutcome, MESSAGE_TYPE, Owed, Station, StationOptions};

/// Turns telecom on for a run when `1`, `on`, `true`, or `yes`, and off
/// when `0`, `off`, `false`, or `no`, whatever `settings.json` says.
pub const ENABLE_ENV: &str = "BAKE_TELECOM";

/// The callsign a session takes when it is free.
pub const CALLSIGN_ENV: &str = "BAKE_TELECOM_CALLSIGN";

/// The callsign of the member that spawned this session.
pub const PARENT_ENV: &str = "BAKE_TELECOM_PARENT";

/// Why a station does not start on this platform.
pub const UNSUPPORTED: &str =
    "telecom needs Unix domain sockets, which this platform's build does not use yet";

/// Whether telecom is on: the environment's choice, else `setting`.
pub fn enabled(setting: bool) -> bool {
    let choice = std::env::var(ENABLE_ENV).ok();
    from_choice(choice.as_deref(), setting)
}

/// The choice [`ENABLE_ENV`] makes for this run, whatever the setting
/// says, or `None` when it makes none.
pub fn env_choice() -> Option<bool> {
    let choice = std::env::var(ENABLE_ENV).ok();
    let on = from_choice(choice.as_deref(), true);
    (on == from_choice(choice.as_deref(), false)).then_some(on)
}

/// [`enabled`] for an [`ENABLE_ENV`] value; any other value leaves
/// `setting`.
fn from_choice(choice: Option<&str>, setting: bool) -> bool {
    match choice
        .map(|value| value.trim().to_ascii_lowercase())
        .as_deref()
    {
        Some("1" | "on" | "true" | "yes") => true,
        Some("0" | "off" | "false" | "no") => false,
        _ => setting,
    }
}

/// The options of a station for a session in `cwd`, from the environment,
/// or `None` when no telecom root can be placed.
pub fn options_from_env(
    cwd: &std::path::Path,
    session_id: &str,
    model: Option<String>,
) -> Option<StationOptions> {
    let home = crate::home::bake_home();
    let root = directory::root(home.as_deref())?;
    let var = |name: &str| {
        std::env::var(name)
            .ok()
            .map(|value| value.trim().to_owned())
            .filter(|value| wire::is_callsign(value))
    };
    Some(StationOptions {
        root,
        cwd: cwd.to_path_buf(),
        session_id: session_id.to_owned(),
        callsign: var(CALLSIGN_ENV),
        parent: var(PARENT_ENV),
        model,
    })
}

#[cfg(test)]
mod tests {
    use super::from_choice;

    #[test]
    fn the_environment_overrides_the_setting_both_ways() {
        assert!(from_choice(Some(" ON "), false));
        assert!(!from_choice(Some("off"), true));
        assert!(from_choice(Some("maybe"), true));
        assert!(!from_choice(None, false));
    }
}
