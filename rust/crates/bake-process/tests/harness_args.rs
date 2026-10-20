#[path = "../../../test-support/harness.rs"]
mod harness;

use harness::HarnessArgs;

fn args(args: &[&str]) -> HarnessArgs {
    HarnessArgs::parse(args.iter().map(|arg| (*arg).to_owned()))
}

#[test]
fn exact_selection_does_not_also_run_a_longer_case_name() {
    let args = args(&["server", "--exact", "--nocapture"]);
    assert!(args.matches("server"));
    assert!(!args.matches("server_shutdown"));
}

#[test]
fn option_values_are_not_test_name_filters() {
    let args = args(&["--list", "--format", "terse", "--color", "never"]);
    assert!(args.matches("server"));
    assert!(args.list(["server"]));
}

#[test]
fn ignored_discovery_and_execution_select_nothing() {
    let args = args(&["--ignored"]);
    assert!(!args.matches("server"));
    assert!(!args.list(["server"]));
    let discovery = self::args(&["--ignored", "--list", "--format", "terse"]);
    assert!(!discovery.matches("server"));
    assert!(discovery.list(["server"]));
}

#[test]
fn substring_filters_are_alternatives_and_skips_win() {
    let args = args(&["server", "client", "--skip", "shutdown"]);
    assert!(args.matches("server_start"));
    assert!(args.matches("client_call"));
    assert!(!args.matches("server_shutdown"));
    assert!(!args.matches("other"));
}
