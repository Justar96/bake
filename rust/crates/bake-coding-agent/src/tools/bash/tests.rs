//! Unit tests for the bash tool's helpers; `tests/bash.rs` covers the tool.

use std::ffi::OsString;
use std::path::Path;
use std::time::Duration;

use serde_json::json;

use super::local::resolve_timeout;
use super::shell::shell_env_from;
use super::{append_status, parse_params, wall_time_seconds};

/// Pi's `resolveTimeoutMs`: positive finite seconds up to 2^31 - 1 ms.
#[test]
fn timeouts_follow_pis_bounds() {
    assert_eq!(resolve_timeout(None), Ok(None));
    assert_eq!(
        resolve_timeout(Some(0.05)),
        Ok(Some(Duration::from_millis(50)))
    );
    assert_eq!(
        resolve_timeout(Some(2_147_483.647)),
        Ok(Some(Duration::from_secs_f64(2_147_483.647)))
    );
    for invalid in [0.0, -1.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        assert_eq!(
            resolve_timeout(Some(invalid)),
            Err("Invalid timeout: must be a finite number of seconds".to_owned())
        );
    }
    assert_eq!(
        resolve_timeout(Some(2_147_483.648)),
        Err("Invalid timeout: maximum is 2147483.647 seconds".to_owned())
    );
    assert_eq!(
        resolve_timeout(Some(f64::MAX)),
        Err("Invalid timeout: maximum is 2147483.647 seconds".to_owned())
    );
}

/// A non-number timeout from a direct call is refused as not finite.
#[test]
fn parameters_tolerate_direct_calls() {
    assert_eq!(
        parse_params(&json!({ "command": "ls" })),
        Ok(("ls".to_owned(), None))
    );
    assert_eq!(
        parse_params(&json!({ "command": "ls", "timeout": null })),
        Ok(("ls".to_owned(), None))
    );
    let (_, timeout) = parse_params(&json!({ "command": "ls", "timeout": "5" })).unwrap();
    assert!(timeout.is_some_and(f64::is_nan));
    assert!(parse_params(&json!({ "command": 5 })).is_err());
    assert!(parse_params(&json!([])).is_err());
}

/// Pi's `Math.round(ms / 100) / 10`.
#[test]
fn wall_time_rounds_to_tenths() {
    assert_eq!(wall_time_seconds(Duration::ZERO), json!(0));
    assert_eq!(wall_time_seconds(Duration::from_millis(49)), json!(0));
    assert_eq!(wall_time_seconds(Duration::from_millis(50)), json!(0.1));
    assert_eq!(wall_time_seconds(Duration::from_millis(1949)), json!(1.9));
    assert_eq!(wall_time_seconds(Duration::from_millis(2000)), json!(2));
}

/// Pi's `appendStatus`.
#[test]
fn status_follows_output_after_a_blank_line() {
    assert_eq!(append_status("", "Command aborted"), "Command aborted");
    assert_eq!(append_status("out\n", "done"), "out\n\n\ndone");
}

#[cfg(unix)]
const SEPARATOR: &str = ":";
#[cfg(windows)]
const SEPARATOR: &str = ";";

/// Pi's `getShellEnv`: the bin directory first on `PATH` (found
/// case-insensitively), once.
#[cfg(any(unix, windows))]
#[test]
fn shell_env_puts_the_bin_dir_first_once() {
    let bin = Path::new("bake-bin");
    let env = |pairs: &[(&str, &str)]| -> Vec<(OsString, OsString)> {
        pairs
            .iter()
            .map(|(key, value)| (OsString::from(key), OsString::from(value)))
            .collect()
    };
    let path = format!("a{SEPARATOR}b");
    assert_eq!(
        shell_env_from(env(&[("Path", &path), ("X", "1")]), Some(bin)),
        env(&[
            ("Path", &format!("bake-bin{SEPARATOR}a{SEPARATOR}b")),
            ("X", "1")
        ])
    );
    let listed = format!("a{SEPARATOR}bake-bin");
    assert_eq!(
        shell_env_from(env(&[("PATH", &listed)]), Some(bin)),
        env(&[("PATH", &listed)])
    );
    assert_eq!(
        shell_env_from(env(&[("X", "1")]), Some(bin)),
        env(&[("X", "1"), ("PATH", "bake-bin")])
    );
    assert_eq!(
        shell_env_from(env(&[("PATH", "")]), Some(bin)),
        env(&[("PATH", "bake-bin")])
    );
    assert_eq!(
        shell_env_from(env(&[("PATH", "a")]), None),
        env(&[("PATH", "a")])
    );
}
